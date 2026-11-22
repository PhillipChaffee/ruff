//! Relations between models: related models of relation fields, reverse accessors and the
//! intermediate model of a many-to-many field.

use ruff_python_ast::name::Name;
use ty_module_resolver::{ImportingFile, KnownModule, ModuleName, file_to_module, resolve_module};
use ty_python_core::ProgramFile;

use crate::place::{global_symbol, known_module_symbol};
use crate::types::call::Binding;
use crate::types::class::{ClassLiteral, DynamicClassAnchor, DynamicClassLiteral};
use crate::types::{ClassBase, KnownClass, Specialization, StaticClassLiteral, Type, UnionType};
use crate::{Db, ProgramEnvironment};

use super::fields::{Field, model_metadata, own_model_fields};
use super::model::concrete_model;
use super::project::project_models;
use super::{inherits_from, is_model};

/// Returns whether `class` is Django's `ForeignKey` or one of its subclasses.
///
/// `OneToOneField` is a subclass of `ForeignKey`, and so is a user-defined relation field:
/// ```py
/// class TrackedForeignKey(models.ForeignKey): ...
/// ```
pub(in crate::types) fn is_foreign_key<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> bool {
    inherits_from(db, class, KnownClass::DjangoForeignKey)
}

/// An attribute that a relation field adds to the model it refers to.
#[derive(Debug, Clone, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
struct ReverseAccessor<'db> {
    pub(super) name: Name,
    pub(super) ty: Type<'db>,
}

/// Synthesize the reverse accessors of a model.
///
/// A `ForeignKey` adds a manager for the source model to the related model, and a
/// `OneToOneField` adds the source model instance. The default name is the lowercase name of the
/// source model, followed by `_set` for a `ForeignKey`:
/// ```py
/// class Author(models.Model):
///     pass
///
/// class Book(models.Model):
///     author = models.ForeignKey(Author, on_delete=models.CASCADE)
///
/// class Profile(models.Model):
///     author = models.OneToOneField(Author, on_delete=models.CASCADE)
///
/// # Author().book_set is a manager for Book; Author().profile has type Profile.
/// ```
///
/// We find the relation fields of the models in the module that defines the related model, and in
/// the project's `models` modules (see [`models_modules`]).
pub(super) fn reverse_accessor<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
    name: &str,
) -> Option<Type<'db>> {
    reverse_accessors(db, class)
        .iter()
        .find(|accessor| accessor.name == name)
        .map(|accessor| accessor.ty)
}

/// All reverse accessors that the project's models add to `class`.
#[salsa::tracked(returns(deref), cycle_initial=|_, _, _| Box::default(), heap_size=ruff_memory_usage::heap_size)]
fn reverse_accessors<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> Box<[ReverseAccessor<'db>]> {
    let file = class.body_scope(db).program_file(db);
    let env = ProgramEnvironment::from_file(file);
    let target = Type::instance(db, &env, class.default_specialization(db));

    let sources = project_models(db, file)
        // Abstract models have no table, so nothing can refer to their rows.
        .filter(|source| !model_metadata(db, *source).is_abstract);

    let mut accessors = Vec::new();
    for source in sources {
        let source_name = source.name(db).to_lowercase();
        let source_ty = Type::instance(db, &env, source.default_specialization(db));

        // A model that inherits from a concrete model is reachable from its parent.
        if source != class
            && !model_metadata(db, source).is_proxy
            && source
                .explicit_bases(db)
                .iter()
                .filter_map(|base| concrete_model(db, base.to_class_type(db)?))
                .any(|parent| {
                    parent.static_class_literal(db).map(|(parent, _)| parent) == Some(class)
                })
        {
            accessors.push(ReverseAccessor {
                name: Name::new(&source_name),
                ty: source_ty,
            });
        }

        // The fields of the model itself, and the ones it inherits from abstract models.
        let declaring_classes = source
            .iter_mro(db, None)
            .filter_map(ClassBase::into_class)
            .filter_map(|base| base.static_class_literal(db))
            .map(|(base, _)| base)
            .filter(|base| {
                *base == source || (is_model(db, *base) && model_metadata(db, *base).is_abstract)
            });
        for declaring in declaring_classes {
            for field in own_model_fields(db, declaring) {
                let Some((field_class, _)) = field
                    .ty
                    .nominal_class(db, &env)
                    .and_then(|field_class| field_class.static_class_literal(db))
                else {
                    continue;
                };
                if !is_foreign_key(db, field_class)
                    || related_model(db, &env, declaring, source, field) != Some(target)
                {
                    continue;
                }
                let is_one_to_one = inherits_from(db, field_class, KnownClass::DjangoOneToOneField);
                let name = match field.metadata.related_name.as_deref() {
                    // A name that ends with `+` disables the reverse accessor. We do not know the
                    // app label of a model.
                    Some(related_name)
                        if related_name.ends_with('+')
                            || related_name.contains("%(app_label)s") =>
                    {
                        continue;
                    }
                    Some(related_name) => related_name.replace("%(class)s", &source_name),
                    None if is_one_to_one => source_name.clone(),
                    None => format!("{source_name}_set"),
                };
                let ty = if is_one_to_one {
                    Some(source_ty)
                } else {
                    related_manager(db, &env, source_ty)
                };
                if let Some(ty) = ty {
                    accessors.push(ReverseAccessor {
                        name: Name::new(name),
                        ty,
                    });
                }
            }
        }
    }
    accessors.into_boxed_slice()
}

/// Return the type of the manager that a `ForeignKey` adds to its related model.
fn related_manager<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    model: Type<'db>,
) -> Option<Type<'db>> {
    let manager = known_module_symbol(
        db,
        env,
        KnownModule::DjangoDbModelsFieldsRelatedDescriptors,
        "RelatedManager",
    )
    .place
    .ignore_possibly_undefined()?
    .as_class_literal()?
    .as_static()?;
    let generic_context = manager.generic_context(db)?;
    (generic_context.len(db) == 2).then(|| {
        Type::instance(
            db,
            env,
            manager.apply_specialization(db, |generic_context| {
                generic_context.specialize_partial(db, [Some(model), None])
            }),
        )
    })
}

/// django-stubs annotates `ForeignKey.__init__`'s `to` parameter as `type[Model] | str`.
/// Use the supplied model class to fill missing write and read types on the descriptor, in that
/// order.
pub(in crate::types) fn foreign_key_model_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    binding: &Binding<'db>,
) -> Option<(Type<'db>, Type<'db>)> {
    let to = binding.parameter_type_by_name(db, "to", false).ok()??;
    let null = binding.parameter_type_by_name(db, "null", false).ok()?;
    let is_nullable = null.and_then(Type::as_bool_literal) == Some(true);
    let model = match to.to_instance_approximation(db, env) {
        Some(model) => model,
        // A string reference such as `ForeignKey("Author", null=True)`. We do not resolve
        // the string to a model, but the field is still nullable: use `Any | None` so that
        // `None` is not lost from the field's type.
        None if is_nullable && to.as_string_literal().is_some() => Type::any(),
        None => return None,
    };
    Some(foreign_key_value_types(db, env, model, is_nullable))
}

/// Return the write and read types of a foreign key to `model`.
///
/// Django checks an assigned instance against the concrete model of the related model, so a
/// foreign key to a proxy model accepts any instance of the model that the proxy is based on, but
/// still returns the proxy model:
/// ```py
/// class BookProxy(Book):
///     class Meta:
///         proxy = True
///
/// class Reader(models.Model):
///     favourite = models.ForeignKey(BookProxy, on_delete=models.CASCADE)
///
/// reader.favourite = Book()  # ok
/// reveal_type(reader.favourite)  # BookProxy
/// ```
fn foreign_key_value_types<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    model: Type<'db>,
    is_nullable: bool,
) -> (Type<'db>, Type<'db>) {
    let write = model
        .nominal_class(db, env)
        .and_then(|class| concrete_model(db, class))
        .map_or(model, |concrete| Type::instance(db, env, concrete));
    let with_none = |ty| {
        if is_nullable {
            UnionType::from_two_elements(db, env, ty, Type::none(db, env))
        } else {
            ty
        }
    };
    (with_none(write), with_none(model))
}

/// Return the model that a relation field of `model` refers to. `declaring` is the class whose
/// body declares the field: `model` itself or one of its abstract base classes.
///
/// The related model is the first argument of the field construction. It can be a class, the
/// name of a model in the same module, or `"self"` for the model that declares the field:
/// ```py
/// class Book(models.Model):
///     author = models.ForeignKey("Author", on_delete=models.CASCADE)
///     sequel = models.ForeignKey("self", null=True, on_delete=models.SET_NULL)
/// ```
///
/// A reference with an app label, such as `"library.Author"`, names a model of another app.
pub(super) fn related_model<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declaring: StaticClassLiteral<'db>,
    model: StaticClassLiteral<'db>,
    field: &Field<'db>,
) -> Option<Type<'db>> {
    model_reference(db, env, declaring, model, field.to?)
}

/// Return the model that `reference` denotes: a model class, the name of a model in the module of
/// `declaring`, or `"self"` for `model`.
fn model_reference<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declaring: StaticClassLiteral<'db>,
    model: StaticClassLiteral<'db>,
    reference: Type<'db>,
) -> Option<Type<'db>> {
    let to = reference;
    let related = match to.as_string_literal() {
        Some(reference) if reference.value(db) == "self" => {
            return Some(Type::instance(db, env, model.default_specialization(db)));
        }
        Some(reference) => {
            let file = declaring.body_scope(db).program_file(db);
            match reference.value(db).split_once('.') {
                Some((app_label, name)) => app_model_symbol(db, env, file, app_label, name)?,
                None => global_symbol(db, file, reference.value(db))
                    .place
                    .ignore_possibly_undefined()?,
            }
        }
        None => to,
    }
    .to_instance_approximation(db, env)?;

    let (related_class, _) = related.nominal_class(db, env)?.static_class_literal(db)?;
    is_model(db, related_class).then_some(related)
}

/// Look up `name` in the `models` module of the app with the label `app_label`, for a reference
/// such as `"library.Author"` in `file`.
///
/// An app's label is by default the last component of its package name, and its models are
/// importable from the `models` module of that package. We do not read the project's settings to
/// find the installed apps. Instead, we look for a package named `app_label` next to each package
/// that contains `file`, starting with the innermost. For a reference in `shop/reviews/models.py`,
/// we try `shop.reviews.library.models`, `shop.library.models` and then `library.models`.
///
/// An app that sets a different label in its `AppConfig` is not found.
fn app_model_symbol<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    file: ProgramFile<'db>,
    app_label: &str,
    name: &str,
) -> Option<Type<'db>> {
    let models = ModuleName::from_components([app_label, "models"])?;
    let importing_file = ImportingFile::File(file.file(db), env.resolver_environment(db));
    let containing_packages = file_to_module(db, file.resolver_file(db))?
        .name(db)
        .ancestors()
        .skip(1);

    containing_packages
        .map(|mut package| {
            package.extend(&models);
            package
        })
        .chain(std::iter::once(models.clone()))
        .find_map(|candidate| {
            let module = resolve_module(db, importing_file, &candidate)?.file(db)?;
            global_symbol(db, ProgramFile::new(db, module, env.program(db)), name)
                .place
                .ignore_possibly_undefined()
        })
}

/// Return a more precise type for the relation field `name` of the model `class`, if the stubs
/// could not determine its related model.
///
/// `ty` is the type of the field according to the stubs. django-stubs infers the related model
/// from the type arguments of the field class, which are unknown for a string reference and do
/// not exist for a subclass that is not generic:
/// ```py
/// class TrackedForeignKey(models.ForeignKey): ...
///
/// class Book(models.Model):
///     author = models.ForeignKey("Author", on_delete=models.CASCADE)
///     editor = TrackedForeignKey(Author, on_delete=models.CASCADE)
/// ```
///
/// Both fields get the type `ForeignKey[Author, Author]`.
pub(in crate::types) fn refined_field_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    let (field_class, specialization) = ty.nominal_class(db, env)?.static_class_literal(db)?;
    if inherits_from(db, field_class, KnownClass::DjangoManyToManyField) {
        // A subclass that does not declare type parameters is refined as its generic base class.
        let (generic_class, specialization) = if field_class.generic_context(db).is_some() {
            (field_class, specialization)
        } else {
            ty.nominal_class(db, env)?
                .iter_mro(db)
                .filter_map(ClassBase::into_class)
                .filter_map(|base| base.static_class_literal(db))
                .find(|(base, _)| base.generic_context(db).is_some())?
        };
        return refined_many_to_many_type(db, env, class, name, generic_class, specialization?);
    }
    // A related model that the stubs or the constructor already determined takes precedence.
    if specialization.is_some_and(|specialization| {
        specialization.types(db).iter().any(|ty| {
            !ty.filter_union(db, env, |element| !element.is_none(db))
                .is_dynamic()
        })
    }) || !is_foreign_key(db, field_class)
        || !is_model(db, class)
    {
        return None;
    }

    let field = own_model_fields(db, class)
        .iter()
        .find(|field| field.name == name)?;
    let model = related_model(db, env, class, class, field)?;
    let (write, read) = foreign_key_value_types(db, env, model, field.metadata.null);

    // Specialize the field class itself, or its nearest generic base class if it is not generic.
    let generic_class = field_class
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .map(|(base, _)| base)
        .find(|base| {
            base.generic_context(db)
                .is_some_and(|generic_context| generic_context.len(db) == 2)
        })?;
    Some(Type::instance(
        db,
        env,
        generic_class.apply_specialization(db, |generic_context| {
            generic_context.specialize(db, vec![write, read])
        }),
    ))
}

/// Return a more precise type for the `ManyToManyField` `name` of the model `class`, if the
/// related model or the model of the intermediate table is given as a string reference:
/// ```py
/// class Book(models.Model):
///     tags = models.ManyToManyField("Tag", through="Tagging")
/// ```
fn refined_many_to_many_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
    field_class: StaticClassLiteral<'db>,
    specialization: Specialization<'db>,
) -> Option<Type<'db>> {
    let &[to, through] = specialization.types(db) else {
        return None;
    };
    if !(to.is_dynamic() || through.is_dynamic()) || !is_model(db, class) {
        return None;
    }
    let field = own_model_fields(db, class)
        .iter()
        .find(|field| field.name == name)?;
    let resolve = |current: Type<'db>, reference: Option<Type<'db>>| {
        if current.is_dynamic() {
            reference.and_then(|reference| model_reference(db, env, class, class, reference))
        } else {
            None
        }
    };
    let refined_to = resolve(to, field.to);
    let refined_through = match field.through {
        None if through.is_dynamic() => {
            automatic_through_model(db, env, class, field, refined_to.unwrap_or(to))
        }
        reference => resolve(through, reference),
    };
    if refined_to.is_none() && refined_through.is_none() {
        return None;
    }
    Some(Type::instance(
        db,
        env,
        field_class.apply_specialization(db, |generic_context| {
            generic_context.specialize(
                db,
                vec![refined_to.unwrap_or(to), refined_through.unwrap_or(through)],
            )
        }),
    ))
}

/// Return the model of the intermediate table that Django creates for the `ManyToManyField`
/// `field` of the model `class` without a `through` argument. `to` is the related model.
///
/// The model is named after the model and the field, and has a foreign key to each of the two
/// models, named after them:
/// ```py
/// class Book(models.Model):
///     tags = models.ManyToManyField(Tag)
///
/// # Book.tags.through is the model `Book_tags`, with the foreign keys `book` and `tag`.
/// ```
///
/// For a relation from a model to itself, the foreign keys are named `from_<model>` and
/// `to_<model>`. The model has the attributes of the foreign keys and of the primary key, with
/// the types that they have on an instance.
fn automatic_through_model<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    field: &Field<'db>,
    to: Type<'db>,
) -> Option<Type<'db>> {
    // A field of an abstract model creates a separate model for each concrete subclass.
    if model_metadata(db, class).is_abstract {
        return None;
    }
    let (to_class, _) = to.nominal_class(db, env)?.static_class_literal(db)?;
    let source = Type::instance(db, env, class.default_specialization(db));
    let primary_key = |model: Type<'db>| {
        model
            .member(db, env, "pk")
            .place
            .ignore_possibly_undefined()
    };

    let source_name = class.name(db).to_lowercase();
    let to_name = to_class.name(db).to_lowercase();
    let (source_name, to_name) = if source_name == to_name {
        (format!("from_{source_name}"), format!("to_{to_name}"))
    } else {
        (source_name, to_name)
    };
    let id = KnownClass::Int.to_instance(db, env);
    let members: Box<[(Name, Type<'db>)]> = Box::new([
        (Name::new_static("id"), id),
        (Name::new_static("pk"), id),
        (Name::new(format!("{source_name}_id")), primary_key(source)?),
        (Name::new(format!("{to_name}_id")), primary_key(to)?),
        (Name::new(source_name), source),
        (Name::new(to_name), to),
    ]);

    let through = DynamicClassLiteral::new(
        db,
        Name::new(format!("{}_{}", class.name(db), field.name)),
        DynamicClassAnchor::Implicit {
            owner: class,
            explicit_bases: Box::new([KnownClass::DjangoModel.to_class_literal(db, env)]),
        },
        members,
        false,
        None,
    );
    Some(Type::instance(
        db,
        env,
        ClassLiteral::Dynamic(through).default_specialization(db),
    ))
}

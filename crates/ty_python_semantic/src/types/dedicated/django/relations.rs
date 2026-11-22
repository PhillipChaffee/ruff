//! Relations between models: related models of relation fields, reverse accessors and the
//! intermediate model of a many-to-many field.

use ruff_python_ast::name::Name;
use ty_module_resolver::{ImportingFile, KnownModule, ModuleName, file_to_module, resolve_module};
use ty_python_core::ProgramFile;

use crate::place::{global_symbol, known_module_symbol};
use crate::types::call::Binding;
use crate::types::class::{ClassLiteral, DynamicClassAnchor, DynamicClassLiteral};
use crate::types::{
    ClassBase, KnownClass, Specialization, StaticClassLiteral, SubclassOfType, Type, UnionType,
};
use crate::{Db, ProgramEnvironment};

use super::fields::{Field, model_metadata, own_model_fields};
use super::model::concrete_model;
use super::project::{models_modules, project_models};
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
    name: Name,
    ty: Type<'db>,
}

/// A relation to a model from a field of another model, or from a model that inherits from it.
#[derive(Debug, Clone, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) struct IncomingRelation<'db> {
    /// The model that declares or inherits the relation field, or the child model.
    pub(super) source: StaticClassLiteral<'db>,
    /// The attribute that the relation adds to the model, if we synthesize one.
    accessor: Option<ReverseAccessor<'db>>,
    /// The name of the relation in queries, as in `Author.objects.filter(book__title="Dune")`, or
    /// `None` if the relation is hidden.
    pub(super) query_name: Option<Name>,
    /// Whether a row of the model has at most one related row of `source`.
    pub(super) is_one_to_one: bool,
}

/// The relations that the project's models have to a model.
#[derive(Debug, Clone, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) struct IncomingRelations<'db> {
    pub(super) relations: Box<[IncomingRelation<'db>]>,
    /// Whether we know the query names of every relation to the model. A relation whose related
    /// model we cannot resolve might refer to the model, and a name that contains the app label
    /// is unknown, because we do not know the app label of a model.
    pub(super) is_complete: bool,
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
    incoming_relations(db, class)
        .relations
        .iter()
        .filter_map(|relation| relation.accessor.as_ref())
        .find(|accessor| accessor.name == name)
        .map(|accessor| accessor.ty)
}

/// All relations that the project's models have to `class`.
///
/// A relation is named in queries by the `related_query_name` of its field, or else by its
/// `related_name`, or else by the lowercase name of the source model. This differs from the name
/// of the reverse accessor, which ends with `_set` by default for a `ForeignKey`:
/// ```py
/// class Book(models.Model):
///     author = models.ForeignKey(Author, on_delete=models.CASCADE)
///
/// Author().book_set  # the reverse accessor
/// Author.objects.filter(book__title="Dune")  # the query name
/// ```
///
/// Django attaches the relations to a proxy model to the concrete model that the proxy is based
/// on, so these relations have a query name on `class` but no reverse accessor.
#[salsa::tracked(returns(ref), cycle_initial=|_, _, _| IncomingRelations::default(), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn incoming_relations<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> IncomingRelations<'db> {
    let file = class.body_scope(db).program_file(db);
    let env = ProgramEnvironment::from_file(file);
    let target = Type::instance(db, &env, class.default_specialization(db));
    let concrete_target = concrete_model(db, class.default_specialization(db));

    let sources = project_models(db, file)
        // Abstract models have no table, so nothing can refer to their rows.
        .filter(|source| !model_metadata(db, *source).is_abstract);

    let mut relations = Vec::new();
    let mut is_complete = true;
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
            relations.push(IncomingRelation {
                source,
                accessor: Some(ReverseAccessor {
                    name: Name::new(&source_name),
                    ty: source_ty,
                }),
                query_name: Some(Name::new(&source_name)),
                is_one_to_one: true,
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
                let is_many_to_many =
                    inherits_from(db, field_class, KnownClass::DjangoManyToManyField);
                if !(is_foreign_key(db, field_class) || is_many_to_many) {
                    continue;
                }
                let Some(related) = related_model(db, &env, declaring, source, field) else {
                    // An unresolved reference such as `"library.Book"` might name `class`.
                    if field
                        .to
                        .and_then(Type::as_string_literal)
                        .is_some_and(|reference| {
                            let reference = reference.value(db);
                            let model_name = reference
                                .rsplit_once('.')
                                .map_or(reference, |(_, name)| name);
                            model_name.eq_ignore_ascii_case(class.name(db))
                        })
                    {
                        is_complete = false;
                    }
                    continue;
                };
                let is_target = related == target;
                if !is_target
                    && (concrete_target.is_none()
                        || related
                            .nominal_class(db, &env)
                            .and_then(|related| concrete_model(db, related))
                            != concrete_target)
                {
                    continue;
                }

                let is_one_to_one = inherits_from(db, field_class, KnownClass::DjangoOneToOneField);
                let metadata = &field.metadata;
                // A name that ends with `+` hides the relation. We do not know the app label of a
                // model.
                let substitute = |name: &str| {
                    if name.contains("%(app_label)s") {
                        None
                    } else {
                        Some(name.replace("%(class)s", &source_name))
                    }
                };
                let related_name = metadata.related_name.as_deref();
                let query_name = match metadata.related_query_name.as_deref().or(related_name) {
                    Some(name) if name.ends_with('+') => None,
                    Some(name) => {
                        let name = substitute(name);
                        is_complete &= name.is_some();
                        name
                    }
                    None => Some(source_name.clone()),
                };
                let accessor_name = match related_name {
                    Some(name) if name.ends_with('+') => None,
                    Some(name) => substitute(name),
                    None if is_one_to_one => Some(source_name.clone()),
                    None => Some(format!("{source_name}_set")),
                };
                let accessor_ty = if is_one_to_one {
                    Some(source_ty)
                } else if is_many_to_many {
                    None
                } else {
                    related_manager(db, &env, source_ty)
                };
                let accessor =
                    accessor_name
                        .zip(accessor_ty)
                        .filter(|_| is_target)
                        .map(|(name, ty)| ReverseAccessor {
                            name: Name::new(name),
                            ty,
                        });
                relations.push(IncomingRelation {
                    source,
                    accessor,
                    query_name: query_name.map(Name::new),
                    is_one_to_one,
                });
            }
        }
    }
    IncomingRelations {
        relations: relations.into_boxed_slice(),
        is_complete,
    }
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
    };
    model_instance(db, env, related)
}

/// Return the instance type of `class`, if it is a model class.
fn model_instance<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: Type<'db>,
) -> Option<Type<'db>> {
    let instance = class.to_instance_approximation(db, env)?;
    let (model, _) = instance.nominal_class(db, env)?.static_class_literal(db)?;
    is_model(db, model).then_some(instance)
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

/// Return the model that Django's app registry holds under `reference`, `"<app label>.<model>"`,
/// as in the `AUTH_USER_MODEL` setting and in `apps.get_model()`.
///
/// The registry is the same wherever it is used, so unlike [`app_model_symbol`], this does not
/// look near a particular file. We look in each of the project's `models` modules (see
/// [`models_modules`]) whose package has the app label as its last component, which is the label
/// that Django gives an app by default: the app `shop.library` has the label `library`, and its
/// models are in `shop.library.models`.
///
/// An app that sets a different label in its `AppConfig` is not found, and neither are the apps
/// of installed packages, such as `django.contrib.auth`.
pub(super) fn registered_model<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    reference: &str,
) -> Option<Type<'db>> {
    let (app_label, name) = reference.split_once('.')?;
    let program = env.program(db);
    models_modules(db, env.resolver_environment(db))
        .iter()
        .map(|file| ProgramFile::new(db, *file, program))
        .filter(|file| {
            file_to_module(db, file.resolver_file(db)).is_some_and(|module| {
                let module_name = module.name(db);
                module_name.last_component() == "models"
                    && module_name
                        .parent()
                        .is_some_and(|package| package.last_component() == app_label)
            })
        })
        .find_map(|file| {
            let model = global_symbol(db, file, name)
                .place
                .ignore_possibly_undefined()?;
            model_instance(db, env, model)
        })
}

/// Return the type of a call to `Apps.get_model()`, whose overload is `binding`, if it names a
/// model of the project with string literals.
///
/// The stubs return `type[Any]`. The model can be named by the app label and the model name, or
/// by both in one string:
/// ```py
/// reveal_type(apps.get_model("library", "Book"))  # type[Book]
/// reveal_type(apps.get_model("library.Book"))  # type[Book]
/// ```
pub(super) fn get_model_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    binding: &Binding<'db>,
) -> Option<Type<'db>> {
    let app_label = binding
        .parameter_type_by_name(db, "app_label", false)
        .ok()??
        .as_string_literal()?
        .value(db);
    let model_name = binding
        .parameter_type_by_name(db, "model_name", false)
        .ok()?
        .filter(|model_name| !model_name.is_none(db));
    let model = match model_name {
        Some(model_name) => registered_model(
            db,
            env,
            &format!("{app_label}.{}", model_name.as_string_literal()?.value(db)),
        ),
        None => registered_model(db, env, app_label),
    }?;
    Some(SubclassOfType::from(db, env, model.nominal_class(db, env)?))
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

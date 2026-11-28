//! The rows of `values()` and `values_list()`, and the attributes that `annotate()`,
//! `prefetch_related()` and django-stubs-ext's `WithAnnotations` add to model instances.

use ruff_python_ast::{self as ast, name::Name};
use ty_module_resolver::{KnownModule, file_to_module};

use crate::place::known_module_symbol;
use crate::types::call::{Argument, Binding, CallArguments};
use crate::types::class::{
    ClassLiteral, DynamicNamedTupleAnchor, DynamicNamedTupleLiteral, NamedTupleField,
    NamedTupleSpec,
};
use crate::types::context::InferContext;
use crate::types::diagnostic::INVALID_ARGUMENT_TYPE;
use crate::types::type_alias::TypeAliasType;
use crate::types::typed_dict::{TypedDictFieldBuilder, TypedDictType};
use crate::types::{
    ClassBase, ClassType, IntersectionType, KnownClass, Specialization, Type, UnionType,
};
use crate::{Db, ProgramEnvironment};

use super::lookups::{LOOKUP_SEP, QueryContext, resolve_path};
use super::relations::reverse_accessor;
use super::{inherits_from, is_model};

/// Return the type of the instance that a call to the constructor of a Django aggregate such as
/// `Count` creates, with the name that `annotate()` gives the aggregate when it is passed as a
/// positional argument.
///
/// Django names such an annotation after the field of the aggregate and the lowercase name of its
/// function, in the `default_alias` property of the aggregate:
/// ```py
/// Review.objects.annotate(Count("article"))  # each review has an `article__count` attribute
/// ```
///
/// `annotate()` only sees the types of its arguments, so we record the name in the type of the
/// aggregate: `instance`, the instance that the constructor creates according to the stubs, is
/// intersected with a protocol whose `default_alias` is the name as a string literal. Django only
/// has a name for an aggregate of a single field, without a `filter` or `order_by`, which count as
/// further expressions.
pub(in crate::types) fn constructed_instance_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    instance: Type<'db>,
    arguments: &CallArguments<'_, 'db>,
) -> Option<Type<'db>> {
    let mut field = None;
    for argument in arguments.iter() {
        match argument.kind() {
            Argument::Positional if field.is_none() => field = argument.source_type(),
            Argument::Keyword(name) if !matches!(name, "filter" | "order_by") => {}
            _ => return None,
        }
    }
    let field = field?.as_string_literal()?.value(db);

    let (class, _) = instance
        .as_nominal_instance()?
        .class(db, env)
        .static_class_literal(db)?;
    if !inherits_from(db, class, KnownClass::DjangoAggregate) {
        return None;
    }
    // The name of the function is the name of the aggregate class in `django.db.models.aggregates`
    // that a user-defined aggregate inherits from. The two base classes there do not have one.
    let function = class
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .map(|base| base.class_literal(db))
        .find(|base| {
            file_to_module(db, base.program_file(db).resolver_file(db))
                .is_some_and(|module| module.is_known(db, KnownModule::DjangoDbModelsAggregates))
        })?
        .name(db);
    if matches!(function.as_str(), "Aggregate" | "BitAggregate") {
        return None;
    }

    let alias = format!("{field}__{}", function.to_lowercase());
    Some(IntersectionType::from_two_elements(
        db,
        env,
        instance,
        Type::protocol_with_readonly_members(
            db,
            env,
            [("default_alias", Type::string_literal(db, alias.as_str()))],
        ),
    ))
}

/// Add the attributes that `QuerySet.annotate()` or `QuerySet.alias()` creates to the model of
/// the `QuerySet` it returns.
///
/// `annotate()` sets an attribute on each instance for every keyword argument, and for every
/// aggregate passed as a positional argument (see [`constructed_instance_type`]):
/// ```py
/// article = Article.objects.annotate(title_length=Length("title")).get()
/// article.title_length
///
/// review = Review.objects.annotate(Count("article")).get()
/// review.article__count
/// ```
///
/// `alias()` only adds the names to the query, where other methods can use them:
/// ```py
/// Article.objects.alias(title_length=Length("title")).order_by("title_length")
/// ```
///
/// We treat both alike, so that the names of the query are part of its type, although an instance
/// has no attribute for an alias.
///
/// The attributes have the type `Any`, because we do not infer the type of an expression's
/// result.
pub(super) fn annotate_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    receiver: Type<'db>,
    binding: &Binding<'db>,
    arguments: &CallArguments<'_, 'db>,
) -> Option<Type<'db>> {
    let keywords = arguments
        .iter()
        .filter_map(|argument| match argument.kind() {
            Argument::Keyword(name) => Some(name),
            _ => None,
        });
    // The matched arguments include the bound `self` argument.
    let arguments = arguments.with_self(Some(receiver));
    let aggregates = binding
        .signature
        .parameters()
        .variadic()
        .into_iter()
        .flat_map(|(expressions, _)| binding.arguments_for_parameter(&arguments, expressions))
        .filter_map(|(_, ty)| {
            let alias = ty
                .member(db, env, "default_alias")
                .place
                .ignore_possibly_undefined()?;
            Some(alias.as_string_literal()?.value(db))
        });
    annotated_queryset(
        db,
        env,
        binding.return_type(),
        keywords.chain(aggregates).map(|name| (name, Type::any())),
    )
}

/// Add `attributes` to the instances of the `QuerySet` type `declared`.
///
/// `declared` is the type that a `QuerySet` method returns according to the stubs, such as
/// `QuerySet[Article, Article]`. We return a `QuerySet` whose model is annotated with the
/// attributes (see [`annotated_model`]). The rows are annotated too, unless `values()` or
/// `values_list()` changed their shape.
///
/// For a custom `QuerySet` class, we return the intersection of the class with the annotated
/// `QuerySet`, which keeps the methods of the class:
/// ```py
/// class ArticleQuerySet(models.QuerySet["Article"]):
///     def published(self) -> "ArticleQuerySet": ...
///
/// # `ArticleQuerySet & QuerySet[Article & <title_length>, Article & <title_length>]`
/// Article.objects.annotate(title_length=Length("title"))
/// ```
///
/// Returns `None`, which keeps the declared type, when there are no attributes.
fn annotated_queryset<'db, 'a>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declared: Type<'db>,
    attributes: impl IntoIterator<Item = (&'a str, Type<'db>)>,
) -> Option<Type<'db>> {
    let class = declared.nominal_class(db, env)?;
    let &[model, row] =
        base_specialization(db, env, declared, KnownClass::DjangoQuerySet)?.types(db)
    else {
        return None;
    };
    let annotated = annotated_model(db, env, model, attributes)?;

    let queryset = Type::instance(
        db,
        env,
        KnownClass::DjangoQuerySet
            .try_to_class_literal(db, env)?
            .apply_specialization(db, |generic_context| {
                let row = if row == model { annotated } else { row };
                generic_context.specialize(db, vec![annotated, row])
            }),
    );
    if class.is_known(db, KnownClass::DjangoQuerySet) {
        Some(queryset)
    } else {
        Some(IntersectionType::from_two_elements(
            db, env, declared, queryset,
        ))
    }
}

/// Add the attributes that `QuerySet.prefetch_related()` creates for the `Prefetch` objects with a
/// `to_attr` to the model of the `QuerySet` it returns.
///
/// Django stores the related objects of a relation that leads to many objects in a list on each
/// instance, and the related object of a relation that leads to at most one object as it is, or
/// `None` if there is none (`prefetch_one_level()` in `django/db/models/query.py`):
/// ```py
/// author = Author.objects.prefetch_related(Prefetch("books", to_attr="book_list")).get()
/// author.book_list  # list[Book]
///
/// book = Book.objects.prefetch_related(Prefetch("author", to_attr="writer")).get()
/// book.writer  # Author
/// ```
///
/// The related model is the model of the `queryset` of the `Prefetch` object, or else the model
/// that the lookup names (see [`prefetched_relation`]). We do not add an attribute for a lookup
/// that spans a relation, such as `"books__chapters"`, because Django sets it on the related
/// objects.
pub(super) fn prefetch_related_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    receiver: Type<'db>,
    binding: &Binding<'db>,
    arguments: &CallArguments<'_, 'db>,
) -> Option<Type<'db>> {
    let declared = binding.return_type();
    let model = *base_specialization(db, env, declared, KnownClass::DjangoQuerySet)?
        .types(db)
        .first()?;
    let prefetch = known_module_symbol(db, env, KnownModule::DjangoDbModelsQuery, "Prefetch")
        .place
        .ignore_possibly_undefined()?
        .as_class_literal()?
        .as_static()?;

    // The matched arguments include the bound `self` argument.
    let arguments = arguments.with_self(Some(receiver));
    let (lookups, _) = binding.signature.parameters().variadic()?;
    let attributes = binding
        .arguments_for_parameter(&arguments, lookups)
        .filter_map(|(_, ty)| {
            let &[lookup, queryset, to_attr] = ty.specialization_of(db, env, prefetch)?.types(db)
            else {
                return None;
            };
            let to_attr = to_attr.as_string_literal()?.value(db);
            let lookup = lookup.as_string_literal()?.value(db);
            if lookup.contains("__") {
                return None;
            }
            // Without a `queryset`, its type is the default `QuerySet[Model]`.
            let queryset_model = base_specialization(db, env, queryset, KnownClass::DjangoQuerySet)
                .and_then(|specialization| specialization.types(db).first().copied())
                .filter(|related| {
                    !related
                        .nominal_class(db, env)
                        .is_some_and(|class| class.is_known(db, KnownClass::DjangoModel))
                });
            let attribute = match prefetched_relation(db, env, model, lookup) {
                Some(PrefetchedRelation::Many(related)) => KnownClass::List
                    .to_specialized_instance(db, env, &[queryset_model.unwrap_or(related)]),
                // The model of a `queryset` takes precedence over the model of the relation,
                // because Django accepts a queryset of a proxy or child model of the related
                // model. A `queryset` can also leave out the related object.
                Some(PrefetchedRelation::Single(related)) => match queryset_model {
                    Some(queryset_related) => {
                        UnionType::from_two_elements(db, env, queryset_related, Type::none(db, env))
                    }
                    None => related,
                },
                // We do not know whether Django stores a list or a single object, for example for
                // a `GenericForeignKey`.
                None => Type::any(),
            };
            Some((to_attr, attribute))
        })
        .collect::<Vec<_>>();
    annotated_queryset(db, env, declared, attributes)
}

/// What a relation of a model that `prefetch_related()` follows leads to.
enum PrefetchedRelation<'db> {
    /// Any number of objects of the model, through a related manager: a reverse accessor of a
    /// `ForeignKey`, or either side of a `ManyToManyField`.
    Many(Type<'db>),
    /// At most one object, with the type that Django stores for it in the attribute: a
    /// `ForeignKey` or `OneToOneField` of the model, whose value is `None` only if the field
    /// allows NULL, or the reverse accessor of a `OneToOneField`, which can have no related
    /// object.
    Single(Type<'db>),
}

/// Return what the relation `lookup`, an attribute of the instance type `model`, leads to.
fn prefetched_relation<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    model: Type<'db>,
    lookup: &str,
) -> Option<PrefetchedRelation<'db>> {
    let related = model
        .member(db, env, lookup)
        .place
        .ignore_possibly_undefined()?;
    if let Some(manager) = base_specialization(db, env, related, KnownClass::DjangoBaseManager) {
        return Some(PrefetchedRelation::Many(*manager.types(db).first()?));
    }
    let is_model_instance = related
        .filter_union(db, env, |element| !element.is_none(db))
        .nominal_class(db, env)
        .and_then(|class| class.static_class_literal(db))
        .is_some_and(|(class, _)| is_model(db, class));
    if !is_model_instance {
        return None;
    }
    // A reverse accessor that is not a manager belongs to a `OneToOneField`, or to the link from
    // a child model to its parent, which is also a `OneToOneField`.
    let is_reverse_one_to_one = model
        .nominal_class(db, env)?
        .iter_mro(db)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .any(|(base, _)| is_model(db, base) && reverse_accessor(db, base, lookup).is_some());
    Some(PrefetchedRelation::Single(if is_reverse_one_to_one {
        UnionType::from_two_elements(db, env, related, Type::none(db, env))
    } else {
        related
    }))
}

/// Return the type arguments of the class `known` in the MRO of the class of the instance type
/// `ty`, such as the model and row types of a custom `QuerySet` class.
fn base_specialization<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    ty: Type<'db>,
    known: KnownClass,
) -> Option<Specialization<'db>> {
    ty.nominal_class(db, env)?
        .iter_mro(db)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .find(|(base, _)| base.is_known(db, known))?
        .1
}

/// Return the value of `WithAnnotations[Article, TitleLength]` from django-stubs-ext, the model
/// `Article` with the items of the `TypedDict` `TitleLength` as attributes:
/// ```py
/// class TitleLength(TypedDict):
///     title_length: int
///
/// def _(article: WithAnnotations[Article, TitleLength]):
///     article.title_length  # int
/// ```
///
/// django-stubs-ext declares the alias as `Annotated[_T, Annotations[_Annotations_co]]`, whose
/// value is the model alone, because the metadata of `Annotated` is not part of the type. We give
/// it the same type as a model that `annotate()` adds the attributes to.
///
/// Returns `None`, which keeps the value of the alias, for other aliases and for an alias that is
/// not specialized with a model and a `TypedDict`.
pub(in crate::types) fn type_alias_value_override<'db>(
    db: &'db dyn Db,
    alias: TypeAliasType<'db>,
) -> Option<Type<'db>> {
    // Check the name first to avoid a module-resolution query for every type alias.
    if alias.name(db) != "WithAnnotations" {
        return None;
    }
    let definition = alias.definition(db);
    let file = definition.program_file(db);
    if !file_to_module(db, file.resolver_file(db))
        .is_some_and(|module| module.is_known(db, KnownModule::DjangoStubsExtAnnotations))
    {
        return None;
    }
    let &[model, Type::TypedDict(annotations)] = alias.specialization(db)?.types(db) else {
        return None;
    };
    let env = ProgramEnvironment::from_file(file);
    annotated_model(
        db,
        &env,
        model,
        annotations
            .items(db)
            .iter()
            .map(|(name, field)| (name.as_str(), field.declared_ty)),
    )
}

/// Add `attributes` to the model instance type `model`, as the intersection of the model with a
/// protocol that has one read-only member per attribute.
///
/// Returns `None` when there are no attributes.
fn annotated_model<'db, 'a>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    model: Type<'db>,
    attributes: impl IntoIterator<Item = (&'a str, Type<'db>)>,
) -> Option<Type<'db>> {
    let mut attributes = attributes.into_iter().peekable();
    attributes.peek()?;
    Some(IntersectionType::from_two_elements(
        db,
        env,
        model,
        Type::protocol_with_readonly_members(db, env, attributes),
    ))
}

/// The shape of the rows that a `QuerySet` method returns for the fields named in its call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuerySetRows {
    /// `values("title")`: a dictionary keyed by field name.
    Dict,
    /// `values_list("title", "rating")`: a tuple with one element per field.
    Tuple,
    /// `values_list("title", flat=True)`: the value of the single field.
    Flat,
    /// `values_list("title", "rating", named=True)`: a named tuple with one field per field.
    Named,
}

/// Infer the row type of `QuerySet.values()` and `QuerySet.values_list()` from the fields named
/// in the call.
///
/// The stubs declare the rows as `dict[str, Any]` and `Any`. When every field is named by a
/// string literal, the rows follow those fields' types:
/// ```py
/// class Article(models.Model):
///     title = models.CharField(max_length=100)
///     rating = models.IntegerField()
///
/// Article.objects.values("title")  # rows are a `TypedDict` with a `title` key of type `str`
/// Article.objects.values_list("title", "rating")  # rows are `tuple[str, int]`
/// Article.objects.values_list("title", flat=True)  # rows are `str`
/// Article.objects.values_list("title", named=True)  # rows are a named tuple with a `title: str`
/// ```
///
/// A name can follow relations to the fields of other models (see [`resolve_path`]):
/// ```py
/// Book.objects.values_list("author__name", flat=True)  # rows are `str`
/// ```
///
/// Returns `None`, which keeps the declared type, for calls that this does not model: a call
/// without fields, a field that is not a string literal, and a name that ends with a transform,
/// such as `"title__length"`.
fn queryset_rows_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declared: Type<'db>,
    fields: impl IntoIterator<Item = Type<'db>>,
    rows: QuerySetRows,
) -> Option<Type<'db>> {
    let specialization = declared.known_specialization(db, env, KnownClass::DjangoQuerySet)?;
    let queryset = KnownClass::DjangoQuerySet.try_to_class_literal(db, env)?;
    let model = specialization.types(db).first().copied()?;

    let columns = fields
        .into_iter()
        .map(|field| {
            let name = field.as_string_literal()?.value(db);
            let ty = if name.contains(LOOKUP_SEP) {
                let (model, _) = model.nominal_class(db, env)?.static_class_literal(db)?;
                resolve_path(db, &QueryContext::of_model(model), name).value_type(db, env)?
            } else {
                // The member of the model, which can also be an annotation.
                model
                    .member(db, env, name)
                    .place
                    .ignore_possibly_undefined()?
            };
            Some((Name::new(name), ty))
        })
        .collect::<Option<Vec<_>>>()?;
    let row = match (rows, columns.as_slice()) {
        (_, []) => return None,
        (QuerySetRows::Dict, _) => Type::TypedDict(TypedDictType::from_schema_items(
            db,
            columns
                .into_iter()
                .map(|(name, ty)| (name, TypedDictFieldBuilder::new(ty).required(true).build()))
                .collect(),
        )),
        (QuerySetRows::Tuple, _) => {
            Type::heterogeneous_tuple(db, env, columns.into_iter().map(|(_, ty)| ty))
        }
        (QuerySetRows::Flat, [(_, ty)]) => *ty,
        (QuerySetRows::Flat, _) => return None,
        (QuerySetRows::Named, _) => {
            // Django creates a namedtuple class named `Row` for the names of each query.
            let (owner, _) = model.nominal_class(db, env)?.static_class_literal(db)?;
            let fields = columns
                .into_iter()
                .map(|(name, ty)| NamedTupleField {
                    name,
                    ty,
                    default: None,
                    definition: None,
                })
                .collect();
            let row = DynamicNamedTupleLiteral::new(
                db,
                Name::new_static("Row"),
                DynamicNamedTupleAnchor::Implicit {
                    owner,
                    spec: NamedTupleSpec::known(db, fields),
                },
            );
            Type::instance(
                db,
                env,
                ClassType::NonGeneric(ClassLiteral::DynamicNamedTuple(row)),
            )
        }
    };

    Some(Type::instance(
        db,
        env,
        queryset.apply_specialization(db, |generic_context| {
            generic_context.specialize(db, vec![model, row])
        }),
    ))
}

/// Return the type of a call to `values()` or `values_list()` on the Django manager or queryset
/// `receiver`. `binding` is the matched overload of the call.
pub(super) fn values_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    receiver: Type<'db>,
    binding: &Binding<'db>,
    arguments: &CallArguments<'_, 'db>,
) -> Option<Type<'db>> {
    let parameters = binding.signature.parameters();
    // The matched arguments include the bound `self` argument.
    let arguments = arguments.with_self(Some(receiver));
    let rows = if let Some((expressions, _)) = parameters.keyword_variadic() {
        // `values("title", n=Length("title"))` names an expression, whose type we do not model.
        binding
            .arguments_for_parameter(&arguments, expressions)
            .next()
            .is_none()
            .then_some(QuerySetRows::Dict)?
    } else if let Ok(flat) = binding.parameter_type_by_name(db, "flat", false)
        && let Ok(named) = binding.parameter_type_by_name(db, "named", false)
    {
        // An argument that is not a `bool` literal leaves the shape of the rows unknown.
        let is_set =
            |argument: Option<Type<'db>>| argument.map_or(Some(false), Type::as_bool_literal);
        match (is_set(flat)?, is_set(named)?) {
            (false, false) => QuerySetRows::Tuple,
            (true, false) => QuerySetRows::Flat,
            (false, true) => QuerySetRows::Named,
            // Django raises a `TypeError`.
            (true, true) => return None,
        }
    } else {
        return None;
    };
    let (fields, _) = parameters.variadic()?;
    queryset_rows_return_type(
        db,
        env,
        binding.return_type(),
        binding
            .arguments_for_parameter(&arguments, fields)
            .map(|(_, ty)| ty),
        rows,
    )
}

/// Report a combination of the arguments of `values_list()` that Django rejects with a
/// `TypeError`: `flat=True` with more than one field, and `flat=True` with `named=True`.
/// ```py
/// Article.objects.values_list("title", "rating", flat=True)
/// Article.objects.values_list("title", flat=True, named=True)
/// ```
///
/// `binding` is the matched overload of the call.
pub(super) fn report_values_list_arguments<'db>(
    context: &InferContext<'db, '_>,
    arguments: &ast::Arguments,
    binding: &Binding<'db>,
) {
    let db = context.db();
    let is_true = |name: &str| {
        binding
            .parameter_type_by_name(db, name, false)
            .is_ok_and(|ty| ty.and_then(Type::as_bool_literal) == Some(true))
    };
    if !is_true("flat") {
        return;
    }
    let Some(flat) = arguments.find_keyword("flat") else {
        return;
    };
    let message = if is_true("named") {
        "'flat' and 'named' can't be used together"
    } else if arguments.args.len() > 1 && !arguments.args.iter().any(ast::Expr::is_starred_expr) {
        "'flat' is not valid when values_list is called with more than one field"
    } else {
        return;
    };
    if let Some(builder) = context.report_lint(&INVALID_ARGUMENT_TYPE, flat) {
        builder.into_diagnostic(message);
    }
}

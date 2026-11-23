//! Field paths such as `author__name`, which name a field of a model or, through relations, of
//! another model.

use ruff_python_ast::{self as ast, name::Name};
use ty_module_resolver::file_to_module;

use crate::types::context::InferContext;
use crate::types::diagnostic::INVALID_ARGUMENT_TYPE;
use crate::types::{ClassBase, KnownClass, StaticClassLiteral, Type, UnionType};
use crate::{Db, ProgramEnvironment};

use super::fields::{model_metadata, own_model_fields};
use super::project::is_project_search_path;
use super::relations::{incoming_relations, is_foreign_key, related_model};
use super::{inherits_from, is_model};

/// The separator between the names of a path, as in `author__name`.
pub(super) const LOOKUP_SEP: &str = "__";

/// What a name of a model refers to in a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) enum ModelNameKind<'db> {
    /// A field that holds a value, such as `title`, `author_id` or `pk`. A name that follows it in
    /// a path is a transform or a lookup, such as `length` in `title__length`.
    Field { null: bool },
    /// A relation field of the model: a foreign key, a one-to-one field or a many-to-many field.
    /// `related` is the related model, or `None` if we cannot resolve it.
    ForwardRelation {
        related: Option<StaticClassLiteral<'db>>,
        null: bool,
        is_many: bool,
    },
    /// A relation from a field of the model `related` to this model, or from a model that
    /// inherits from it.
    ReverseRelation {
        related: StaticClassLiteral<'db>,
        is_one_to_one: bool,
    },
}

impl ModelNameKind<'_> {
    /// Returns whether a row can have no value for this name, because the field allows NULL or
    /// because the row can have no related row.
    pub(super) fn is_nullable(self) -> bool {
        match self {
            Self::Field { null } => null,
            Self::ForwardRelation { null, is_many, .. } => null || is_many,
            Self::ReverseRelation { .. } => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
struct ModelName<'db> {
    name: Name,
    kind: ModelNameKind<'db>,
}

/// The names that a path can use on a model.
#[derive(Debug, Clone, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
struct ModelNames<'db> {
    names: Box<[ModelName<'db>]>,
    /// Whether we see every name of the model. Otherwise, a name that is not in `names` can still
    /// exist.
    is_complete: bool,
}

impl<'db> ModelNames<'db> {
    fn get(&self, name: &str) -> Option<ModelNameKind<'db>> {
        self.names
            .iter()
            .find(|model_name| model_name.name == name)
            .map(|model_name| model_name.kind)
    }
}

/// Returns whether `class` is defined in the project, rather than in an installed package.
fn is_project_model<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>) -> bool {
    file_to_module(db, class.program_file(db).resolver_file(db))
        .and_then(|module| module.search_path(db))
        .is_some_and(is_project_search_path)
}

/// The names that a path can use on the model `model`: its fields, the fields that it inherits,
/// `pk`, and the relations from other models.
///
/// We only claim to see every name of a model that the project defines, with only project models
/// between it and `Model`. A model of an installed app can have relations from models that we do
/// not see, and its fields are declared in stubs, where we do not read them.
#[salsa::tracked(returns(ref), cycle_initial=|_, _, _| ModelNames::default(), heap_size=ruff_memory_usage::heap_size)]
fn model_names<'db>(db: &'db dyn Db, model: StaticClassLiteral<'db>) -> ModelNames<'db> {
    let env = ProgramEnvironment::from_scope(model.body_scope(db));
    let mut names = vec![ModelName {
        name: Name::new_static("pk"),
        kind: ModelNameKind::Field { null: false },
    }];
    let mut is_complete = true;
    let mut has_primary_key = false;

    for base in model.iter_mro(db, None) {
        let base = match base {
            ClassBase::Class(base) => base,
            // An unknown base class can add fields that we cannot see.
            ClassBase::Dynamic(_) => {
                is_complete = false;
                continue;
            }
            _ => continue,
        };
        let Some((base, _)) = base.static_class_literal(db) else {
            is_complete = false;
            continue;
        };
        if base.is_known(db, KnownClass::DjangoModel) {
            break;
        }
        // Django ignores the fields of a base class that is not a model.
        if !is_model(db, base) {
            continue;
        }
        is_complete &= is_project_model(db, base);

        for field in own_model_fields(db, base) {
            has_primary_key |= field.metadata.primary_key;
            let null = field.metadata.null;
            let field_class = field
                .ty
                .nominal_class(db, &env)
                .and_then(|field_class| field_class.static_class_literal(db))
                .map(|(field_class, _)| field_class);
            let is_foreign_key = field_class.is_some_and(|class| is_foreign_key(db, class));
            let is_many = field_class
                .is_some_and(|class| inherits_from(db, class, KnownClass::DjangoManyToManyField));
            if !(is_foreign_key || is_many) {
                names.push(ModelName {
                    name: field.name.clone(),
                    kind: ModelNameKind::Field { null },
                });
                continue;
            }
            let related = related_model(db, &env, base, model, field)
                .and_then(|related| related.nominal_class(db, &env)?.static_class_literal(db))
                .map(|(related, _)| related);
            names.push(ModelName {
                name: field.name.clone(),
                kind: ModelNameKind::ForwardRelation {
                    related,
                    null,
                    is_many,
                },
            });
            if is_foreign_key {
                names.push(ModelName {
                    name: Name::new(format!("{}_id", field.name)),
                    kind: ModelNameKind::Field { null },
                });
            }
        }

        if model_metadata(db, base).is_abstract {
            continue;
        }
        // A model that inherits from a concrete model links to it through `<parent>_ptr`.
        if base != model {
            let parent = base.name(db).to_lowercase();
            names.push(ModelName {
                name: Name::new(format!("{parent}_ptr")),
                kind: ModelNameKind::ForwardRelation {
                    related: Some(base),
                    null: false,
                    is_many: false,
                },
            });
            names.push(ModelName {
                name: Name::new(format!("{parent}_ptr_id")),
                kind: ModelNameKind::Field { null: false },
            });
        }
        // A model also has the relations to its concrete parents.
        let incoming = incoming_relations(db, base);
        is_complete &= incoming.is_complete;
        names.extend(incoming.relations.iter().filter_map(|relation| {
            Some(ModelName {
                name: relation.query_name.clone()?,
                kind: ModelNameKind::ReverseRelation {
                    related: relation.source,
                    is_one_to_one: relation.is_one_to_one,
                },
            })
        }));
    }
    if !has_primary_key {
        names.push(ModelName {
            name: Name::new_static("id"),
            kind: ModelNameKind::Field { null: false },
        });
    }

    ModelNames {
        names: names.into_boxed_slice(),
        is_complete,
    }
}

/// One name of a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PathStep<'a, 'db> {
    pub(super) name: &'a str,
    /// The model that has the name and what the name refers to, or `None` for a name that we do
    /// not check: a name after a field that holds a value, after a relation whose related model
    /// we cannot resolve, or of a model whose names we do not all see.
    pub(super) resolved: Option<(StaticClassLiteral<'db>, ModelNameKind<'db>)>,
}

/// Why Django cannot resolve a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathError<'a, 'db> {
    /// `model` has no field or relation named `name`.
    UnknownName {
        model: StaticClassLiteral<'db>,
        name: &'a str,
    },
}

/// A path such as `author__name`, resolved name by name, starting from a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedPath<'a, 'db> {
    /// The names up to the first name that cannot be resolved.
    pub(super) steps: Box<[PathStep<'a, 'db>]>,
    pub(super) error: Option<PathError<'a, 'db>>,
}

impl<'db> ResolvedPath<'_, 'db> {
    /// Return the type of the value that the path names, as `values()` returns it, if the path
    /// ends with a field that holds a value. The value can be `None` if a relation on the way can
    /// leave a row without a related row.
    pub(super) fn value_type(
        &self,
        db: &'db dyn Db,
        env: &ProgramEnvironment<'db>,
    ) -> Option<Type<'db>> {
        if self.error.is_some() {
            return None;
        }
        let (last, relations) = self.steps.split_last()?;
        let (model, ModelNameKind::Field { .. }) = last.resolved? else {
            return None;
        };
        let ty = Type::instance(db, env, model.default_specialization(db))
            .member(db, env, last.name)
            .place
            .ignore_possibly_undefined()?;
        let mut is_nullable = false;
        for step in relations {
            let (_, kind) = step.resolved?;
            is_nullable |= kind.is_nullable();
        }
        Some(if is_nullable {
            UnionType::from_two_elements(db, env, ty, Type::none(db, env))
        } else {
            ty
        })
    }
}

/// The model of a query, and the names that `annotate()` and `alias()` added to the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueryContext<'db> {
    model: StaticClassLiteral<'db>,
    /// The names that a path can start with, besides the names of the model.
    annotations: Box<[Name]>,
}

impl<'db> QueryContext<'db> {
    /// A query of `model` without annotations.
    pub(super) fn of_model(model: StaticClassLiteral<'db>) -> Self {
        Self {
            model,
            annotations: Box::default(),
        }
    }
}

/// Return the query of the Django manager or queryset `receiver`, if we see every name that the
/// query can use: `receiver` is one of Django's own managers or querysets, its rows are instances
/// of the model, and the model is annotated only with names that we see.
///
/// `annotate()` and `alias()` intersect the model with a protocol that has one member per name:
/// ```py
/// books = Book.objects.annotate(review_count=Count("reviews"))  # QuerySet[Book & <Protocol>, ...]
/// books.order_by("review_count")
/// ```
///
/// The stubs declare that `extra()`, whose names we do not see, returns `QuerySet[Any, Any]`.
///
/// A custom manager or `QuerySet` class can add names that are not part of its type, so we do not
/// check its queries:
/// ```py
/// class BookManager(models.Manager["Book"]):
///     def get_queryset(self):
///         return super().get_queryset().annotate(review_count=Count("reviews"))
/// ```
pub(super) fn query_context<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    receiver: Type<'db>,
) -> Option<QueryContext<'db>> {
    // `self` in a method of a custom `QuerySet` class, which its callers can annotate.
    if matches!(receiver, Type::TypeVar(_)) {
        return None;
    }
    let is_django_class = |class: StaticClassLiteral<'db>| {
        file_to_module(db, class.program_file(db).resolver_file(db))
            .is_some_and(|module| module.name(db).first_component() == "django")
    };
    // The classes up to `QuerySet` or `BaseManager` must all be Django's own.
    let django_base = |ty: Type<'db>| {
        for base in ty
            .nominal_class(db, env)?
            .iter_mro(db)
            .filter_map(ClassBase::into_class)
        {
            let (base, specialization) = base.static_class_literal(db)?;
            if !is_django_class(base) {
                return None;
            }
            if base.is_known(db, KnownClass::DjangoQuerySet)
                || base.is_known(db, KnownClass::DjangoBaseManager)
            {
                return Some((base, specialization?));
            }
        }
        None
    };
    let (base, specialization) = django_base(receiver)?;
    let &[model, second] = specialization.types(db) else {
        return None;
    };
    let rows_are_models = if base.is_known(db, KnownClass::DjangoBaseManager) {
        // The second type argument of a manager is its `QuerySet` class.
        django_base(second).is_some()
    } else {
        // `values()` and `values_list()` change the rows, and can name expressions.
        second == model
    };
    if !rows_are_models {
        return None;
    }

    let mut annotations = Vec::new();
    let model = match model {
        Type::NominalInstance(instance) => instance,
        Type::Intersection(intersection) => {
            let mut model = None;
            for element in intersection.positive(db) {
                match element {
                    Type::NominalInstance(instance) if model.is_none() => model = Some(*instance),
                    Type::ProtocolInstance(protocol) => annotations.extend(
                        protocol
                            .interface(db)
                            .members(db)
                            .map(|member| Name::new(member.name())),
                    ),
                    // An element that can add names that we do not see.
                    _ => return None,
                }
            }
            model?
        }
        _ => return None,
    };
    let (model, _) = model.class(db, env).static_class_literal(db)?;
    is_model(db, model).then(|| QueryContext {
        model,
        annotations: annotations.into_boxed_slice(),
    })
}

/// Resolve the path `path`, such as `author__name`, in the query `query`, as Django does for the
/// field names of `values()`, `order_by()` and other `QuerySet` methods.
///
/// Each name is looked up on the model that the previous name leads to: a forward relation leads
/// to its related model, and a relation from another model leads to that model:
/// ```py
/// class Author(models.Model):
///     name = models.CharField(max_length=100)
///
/// class Book(models.Model):
///     title = models.CharField(max_length=200)
///     author = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="books")
///
/// Book.objects.values("author__name")  # `author` is a forward relation, `name` a field
/// Author.objects.values("books__title")  # `books` is the relation from `Book.author`
/// ```
///
/// A relation from another model is named by the `related_query_name` of its field, or else by
/// its `related_name`, or else by the lowercase name of the other model. The names after a field
/// that holds a value are not checked, because they can be transforms or lookups, such as
/// `title__length`. Nor are the names after an annotation, whose type we do not know.
pub(super) fn resolve_path<'a, 'db>(
    db: &'db dyn Db,
    query: &QueryContext<'db>,
    path: &'a str,
) -> ResolvedPath<'a, 'db> {
    let mut steps = Vec::new();
    let mut current = Some(query.model);
    for (index, name) in path.split(LOOKUP_SEP).enumerate() {
        let Some(model) = current else {
            steps.push(PathStep {
                name,
                resolved: None,
            });
            continue;
        };
        let names = model_names(db, model);
        let Some(kind) = names.get(name) else {
            let is_annotation = index == 0
                && query
                    .annotations
                    .iter()
                    .any(|annotation| annotation == name);
            if names.is_complete && !is_annotation {
                return ResolvedPath {
                    steps: steps.into_boxed_slice(),
                    error: Some(PathError::UnknownName { model, name }),
                };
            }
            current = None;
            steps.push(PathStep {
                name,
                resolved: None,
            });
            continue;
        };
        current = match kind {
            ModelNameKind::Field { .. } => None,
            ModelNameKind::ForwardRelation { related, .. } => related,
            ModelNameKind::ReverseRelation { related, .. } => Some(related),
        };
        steps.push(PathStep {
            name,
            resolved: Some((model, kind)),
        });
    }
    ResolvedPath {
        steps: steps.into_boxed_slice(),
        error: None,
    }
}

/// A `QuerySet` method that takes field names as strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldNamesMethod {
    /// `order_by("-title", "author__name")`
    OrderBy,
    /// `only("title")` or `defer("title")`
    OnlyOrDefer,
    /// `select_related("author")`, which only takes relations to a single row.
    SelectRelated,
}

impl FieldNamesMethod {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            "order_by" => Some(Self::OrderBy),
            "only" | "defer" => Some(Self::OnlyOrDefer),
            "select_related" => Some(Self::SelectRelated),
            _ => None,
        }
    }

    /// Return the error that Django raises for the field name `path` in the query `query`, if any.
    fn error<'db>(self, db: &'db dyn Db, query: &QueryContext<'db>, path: &str) -> Option<String> {
        let path = match self {
            // `?` orders randomly, and a name with a `.` names a database column.
            Self::OrderBy if path == "?" || path.contains('.') => return None,
            Self::OrderBy => path.strip_prefix('-').unwrap_or(path),
            Self::OnlyOrDefer | Self::SelectRelated => path,
        };
        let resolved = resolve_path(db, query, path);
        if let Some(PathError::UnknownName { model, name }) = resolved.error {
            return Some(match self {
                Self::OrderBy => format!("Cannot resolve keyword '{name}' into field"),
                Self::OnlyOrDefer => format!("{} has no field named '{name}'", model.name(db)),
                Self::SelectRelated => {
                    format!("Invalid field name(s) given in select_related: '{name}'")
                }
            });
        }
        if self != Self::SelectRelated {
            return None;
        }
        // `select_related()` follows the relations that lead to a single row: forward relations
        // that are not many-to-many, and one-to-one relations from another model.
        for step in &resolved.steps {
            let name = step.name;
            match step.resolved?.1 {
                ModelNameKind::ForwardRelation { is_many: false, .. }
                | ModelNameKind::ReverseRelation {
                    is_one_to_one: true,
                    ..
                } => {}
                ModelNameKind::Field { .. } => {
                    return Some(format!(
                        "Non-relational field given in select_related: '{name}'"
                    ));
                }
                ModelNameKind::ForwardRelation { .. } | ModelNameKind::ReverseRelation { .. } => {
                    return Some(format!(
                        "Invalid field name(s) given in select_related: '{name}'"
                    ));
                }
            }
        }
        None
    }
}

/// Report the field names in the arguments of a call of `method` on a manager or queryset that
/// Django cannot resolve in the query `query`:
/// ```py
/// Book.objects.order_by("title")  # Cannot resolve keyword 'title' into field
/// ```
///
/// Only string literals are checked, and only on models whose names we all see.
pub(super) fn report_field_names<'db>(
    context: &InferContext<'db, '_>,
    method: FieldNamesMethod,
    query: &QueryContext<'db>,
    arguments: &ast::Arguments,
) {
    for argument in &arguments.args {
        if let Some(literal) = argument.as_string_literal_expr()
            && let Some(message) = method.error(context.db(), query, literal.value.to_str())
            && let Some(builder) = context.report_lint(&INVALID_ARGUMENT_TYPE, argument)
        {
            builder.into_diagnostic(message);
        }
    }
}

/// Report the names in the `update_fields` argument of `Model.save()` or `Model.asave()` that are
/// not fields that Django can update on `model`: the fields that hold a value and the forward
/// relations to a single row.
/// ```py
/// book.save(update_fields=["title"])
/// ```
pub(super) fn report_update_fields<'db>(
    context: &InferContext<'db, '_>,
    model: StaticClassLiteral<'db>,
    arguments: &ast::Arguments,
) {
    let db = context.db();
    let Some(update_fields) = arguments.find_keyword("update_fields") else {
        return;
    };
    let elements = match &update_fields.value {
        ast::Expr::List(list) => &list.elts,
        ast::Expr::Tuple(tuple) => &tuple.elts,
        ast::Expr::Set(set) => &set.elts,
        _ => return,
    };
    for element in elements {
        let Some(literal) = element.as_string_literal_expr() else {
            continue;
        };
        let name = literal.value.to_str();
        let resolved = resolve_path(db, &QueryContext::of_model(model), name);
        let is_updatable = resolved.error.is_none()
            && match &*resolved.steps {
                [step] => match step.resolved {
                    // A model whose names we do not all see.
                    None => true,
                    Some((
                        _,
                        ModelNameKind::Field { .. }
                        | ModelNameKind::ForwardRelation { is_many: false, .. },
                    )) => true,
                    Some(_) => false,
                },
                // We do not check the other names of a model whose names we do not all see.
                [first, ..] => first.resolved.is_none(),
                [] => true,
            };
        if !is_updatable && let Some(builder) = context.report_lint(&INVALID_ARGUMENT_TYPE, element)
        {
            builder.into_diagnostic(format_args!(
                "'{name}' is not a field of `{}` that can be updated",
                model.name(db)
            ));
        }
    }
}

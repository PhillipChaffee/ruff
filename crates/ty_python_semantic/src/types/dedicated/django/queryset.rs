//! The rows of `values()` and `values_list()`, and the attributes that `annotate()` adds.

use ruff_python_ast::name::Name;

use crate::types::call::{Binding, CallArguments};
use crate::types::typed_dict::{TypedDictFieldBuilder, TypedDictType};
use crate::types::{IntersectionType, KnownClass, Type};
use crate::{Db, ProgramEnvironment};

/// Add the attributes that `QuerySet.annotate()` creates to the model of the `QuerySet` it
/// returns.
///
/// `annotate()` sets an attribute on each instance for every keyword argument:
/// ```py
/// article = Article.objects.annotate(title_length=Length("title")).get()
/// article.title_length
/// ```
///
/// `declared` is the return type according to the stubs, the same `QuerySet[Article, Article]`
/// that the method is called on. We intersect the model with a protocol that has one member per
/// keyword. The members have the type `Any`, because we do not infer the type of an expression's
/// result.
///
/// Returns `None`, which keeps the declared type, for a `QuerySet` class that fixes its model.
pub(super) fn annotate_return_type<'db, 'a>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declared: Type<'db>,
    keywords: impl IntoIterator<Item = &'a str>,
) -> Option<Type<'db>> {
    let specialization = declared.known_specialization(db, env, KnownClass::DjangoQuerySet)?;
    let queryset = KnownClass::DjangoQuerySet.try_to_class_literal(db, env)?;
    let &[model, row] = specialization.types(db) else {
        return None;
    };

    let mut annotations = keywords
        .into_iter()
        .map(|keyword| (keyword, Type::any()))
        .peekable();
    annotations.peek()?;
    let annotated = IntersectionType::from_two_elements(
        db,
        env,
        model,
        Type::protocol_with_readonly_members(db, env, annotations),
    );

    Some(Type::instance(
        db,
        env,
        queryset.apply_specialization(db, |generic_context| {
            // Rows are model instances unless `values()` or `values_list()` changed their shape.
            let row = if row == model { annotated } else { row };
            generic_context.specialize(db, vec![annotated, row])
        }),
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
/// ```
///
/// Returns `None`, which keeps the declared type, for calls that this does not model: a call
/// without fields, a field that is not a string literal, and a lookup that spans a relation such
/// as `"author__name"`.
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
            let ty = model
                .member(db, env, name)
                .place
                .ignore_possibly_undefined()?;
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
    } else if let Ok(None) = binding.parameter_type_by_name(db, "named", false)
        && let Ok(flat) = binding.parameter_type_by_name(db, "flat", false)
    {
        match flat {
            None => QuerySetRows::Tuple,
            Some(flat) => {
                if flat.as_bool_literal()? {
                    QuerySetRows::Flat
                } else {
                    QuerySetRows::Tuple
                }
            }
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

//! Managers: the methods they copy from their `QuerySet` class, and `Manager.from_queryset()`.

use crate::types::call::Binding;
use crate::types::member::class_member;
use crate::types::{ClassBase, KnownClass, Specialization, Type};
use crate::{Db, ProgramEnvironment};

/// Synthesize the methods that a manager copies from its `QuerySet` class.
///
/// A manager created with `QuerySet.as_manager()` or `Manager.from_queryset()` has a copy of every
/// public method that the custom `QuerySet` class adds. For example, `Book.objects.published()`
/// calls `BookQuerySet.published`:
/// ```py
/// class BookQuerySet(models.QuerySet["Book"]):
///     def published(self) -> "BookQuerySet": ...
///
/// class Book(models.Model):
///     objects = BookQuerySet.as_manager()
/// ```
///
/// django-stubs gives such a manager the type `Manager[Book, BookQuerySet]`, so we look the method
/// up on the second type argument.
pub(super) fn manager_queryset_method<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    specialization: Specialization<'db>,
    name: &str,
) -> Option<Type<'db>> {
    // Django does not copy private methods.
    if name.starts_with('_') {
        return None;
    }
    let queryset_ty = *specialization.types(db).get(1)?;

    // The stubs already declare the methods of `QuerySet` itself on the manager, or leave them
    // out on purpose, so only consider the classes below `QuerySet`.
    queryset_ty
        .nominal_class(db, env)?
        .iter_mro(db)
        .filter_map(ClassBase::into_class)
        .map_while(|base| base.static_class_literal(db))
        .take_while(|(base, _)| !base.is_known(db, KnownClass::DjangoQuerySet))
        .find(|(base, _)| !class_member(db, base.body_scope(db), name).is_undefined())?;

    let method = queryset_ty
        .member(db, env, name)
        .place
        .ignore_possibly_undefined()?;
    matches!(method, Type::BoundMethod(_)).then_some(method)
}

/// Return the manager class that `Manager.from_queryset()` creates.
///
/// django-stubs declares the return type as `type[Self]`, which loses the `QuerySet` class. We
/// specialize the manager with the model and the `QuerySet` class instead, so that the manager
/// has the methods of the custom `QuerySet`:
/// ```py
/// class BookQuerySet(models.QuerySet["Book"]): ...
///
/// # BookManager is Manager[Book, BookQuerySet]
/// BookManager = models.Manager.from_queryset(BookQuerySet)
/// ```
pub(in crate::types) fn from_queryset_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    manager: Type<'db>,
    binding: &Binding<'db>,
) -> Option<Type<'db>> {
    let (manager, _) = manager.to_class_type(db)?.static_class_literal(db)?;
    // A manager class that fixes its own type arguments cannot be specialized again.
    if manager.generic_context(db)?.len(db) != 2
        || !manager
            .iter_mro(db, None)
            .filter_map(ClassBase::into_class)
            .any(|base| base.is_known(db, KnownClass::DjangoBaseManager))
    {
        return None;
    }

    let queryset = binding
        .parameter_type_by_name(db, "queryset_class", false)
        .ok()??
        .to_instance_approximation(db, env)?;
    let model = queryset
        .nominal_class(db, env)?
        .iter_mro(db)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .find(|(base, _)| base.is_known(db, KnownClass::DjangoQuerySet))
        .and_then(|(_, specialization)| specialization?.types(db).first().copied())?;

    Some(Type::from(
        manager.apply_specialization(db, |generic_context| {
            generic_context.specialize(db, vec![model, queryset])
        }),
    ))
}

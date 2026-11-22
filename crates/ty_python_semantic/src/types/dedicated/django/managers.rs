//! Managers: the methods they copy from their `QuerySet` class, `Manager.from_queryset()`, and
//! the keywords of `create()`.

use ty_module_resolver::file_to_module;

use crate::types::call::Binding;
use crate::types::callable::CallableTypeKind;
use crate::types::member::class_member;
use crate::types::signatures::CallableSignature;
use crate::types::{
    CallableType, ClassBase, IntersectionType, KnownClass, ParameterKind, Parameters, Signature,
    Specialization, StaticClassLiteral, Type,
};
use crate::{Db, ProgramEnvironment};

use super::is_model;

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
pub(super) fn from_queryset_return_type<'db>(
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

/// Bind a manager that the model `model` declares in its class body to the model.
///
/// Nothing in `models.Manager()` names the model, so the call creates a
/// `Manager[Model, QuerySet[Model, Model]]`, and a subclass that does not name the model, such as
/// `class BookManager(models.Manager)`, leaves the model unknown. Django binds every manager to the
/// model that declares or inherits it, as if it were written `models.Manager["Book"]()`:
/// ```py
/// class Book(models.Model):
///     objects = models.Manager()  # Manager[Book, QuerySet[Book, Book]]
///     published = BookManager()  # BookManager, with the methods of Manager[Book, QuerySet[Book, Book]]
/// ```
///
/// `ty` is the type of the manager according to the stubs. Returns `None` for anything that is not a
/// manager, and for a manager whose class already names a model.
pub(super) fn bound_manager<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    model: StaticClassLiteral<'db>,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    let manager = ty.nominal_class(db, env)?;
    let bases = || {
        manager
            .iter_mro(db)
            .filter_map(ClassBase::into_class)
            .filter_map(|base| base.static_class_literal(db))
    };
    let declared_model = bases()
        .find(|(base, _)| base.is_known(db, KnownClass::DjangoBaseManager))
        .and_then(|(_, specialization)| specialization?.types(db).first().copied())?;
    let base_model = KnownClass::DjangoModel.to_instance(db, env);
    if !(declared_model.is_dynamic() || declared_model == base_model) || !is_model(db, model) {
        return None;
    }

    // The most derived generic manager class, which is `Manager` itself for `models.Manager()`.
    let (generic_manager, _) = bases().find(|(base, _)| {
        base.generic_context(db).is_some()
            && base
                .iter_mro(db, None)
                .filter_map(ClassBase::into_class)
                .any(|base| base.is_known(db, KnownClass::DjangoBaseManager))
    })?;
    let variables = generic_manager.generic_context(db)?.len(db);
    let model_instance = Type::instance(db, env, model.default_specialization(db));
    let bound = Type::instance(
        db,
        env,
        generic_manager.apply_specialization(db, |generic_context| {
            let mut types = vec![None; variables];
            if let Some(first) = types.first_mut() {
                *first = Some(model_instance);
            }
            generic_context.specialize_partial(db, types)
        }),
    );

    if manager.static_class_literal(db).map(|(manager, _)| manager) == Some(generic_manager) {
        Some(bound)
    } else {
        // A subclass keeps its own methods, and gets the model from the generic manager.
        Some(IntersectionType::from_two_elements(db, env, ty, bound))
    }
}

/// Return the manager `name` that the model `model` inherits from a base class, bound to `model`.
///
/// Django gives each model its own copy of the managers that its base classes declare, bound to the
/// model:
/// ```py
/// class Publication(models.Model):
///     objects = models.Manager()
///
///     class Meta:
///         abstract = True
///
/// class Magazine(Publication): ...
///
/// # Magazine.objects is a Manager[Magazine, QuerySet[Magazine, Magazine]].
/// ```
pub(super) fn inherited_manager<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    model: StaticClassLiteral<'db>,
    name: &str,
) -> Option<Type<'db>> {
    // The first base class that declares `name` decides, whether or not it is a manager.
    let declared = model
        .iter_mro(db, None)
        .skip(1)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .find_map(|(base, _)| {
            class_member(db, base.body_scope(db), name).ignore_possibly_undefined()
        })?;
    bound_manager(db, env, model, declared)
}

/// Give `create()` and `acreate()` of a manager or queryset the keyword parameters of the model's
/// constructor.
///
/// Django's `QuerySet.create(**kwargs)` calls `self.model(**kwargs)` and saves the result, so
/// `Book.objects.create(title="Dune")` raises the same `TypeError` as `Book(title="Dune")`. The
/// stubs declare `def create(self, **kwargs: Any) -> _T_co`, which accepts any keyword. We replace
/// `**kwargs` with the keyword parameters of the model's `__init__`: the one that we synthesize from
/// the fields, or the one that the model defines.
/// ```py
/// class Book(models.Model):
///     title = models.CharField(max_length=200)
///
/// # Book.objects.create is `(*, title: str | int | Combinable = ..., id: Any = ..., pk: Any = ...) -> Book`
/// ```
///
/// The other parameters of the declared method, such as `through_defaults` of a many-to-many
/// manager, and its return type are kept. `class` is the class that declares the method, and
/// `specialization` its type arguments, which name the model.
///
/// Returns `None`, which keeps the declared method, for a method that is not Django's own, such as
/// an override in a custom manager, and when the model is not known.
pub(super) fn create_method<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    let Type::FunctionLiteral(function) = ty else {
        return None;
    };
    let module = file_to_module(db, class.program_file(db).resolver_file(db))?;
    if module.name(db).first_component() != "django" {
        return None;
    }
    // The type arguments of `BaseManager` or `QuerySet`, whichever `class` inherits from.
    let base_specialization = |known: KnownClass| {
        class
            .iter_mro(db, specialization)
            .filter_map(ClassBase::into_class)
            .filter_map(|base| base.static_class_literal(db))
            .find(|(base, _)| base.is_known(db, known))
            .and_then(|(_, specialization)| specialization)
    };
    let base = if let Some(manager) = base_specialization(KnownClass::DjangoBaseManager) {
        // A manager created with `as_manager()` or `from_queryset()` has a copy of a `create()`
        // that the custom `QuerySet` class defines, which replaces the one declared on `Manager`.
        if let Some(method) = manager_queryset_method(db, env, manager, function.name(db).as_str())
        {
            return Some(method);
        }
        manager
    } else {
        base_specialization(KnownClass::DjangoQuerySet)?
    };
    let model = *base.types(db).first()?;
    if !model
        .nominal_class(db, env)?
        .static_class_literal(db)
        .is_some_and(|(model, _)| is_model(db, model))
    {
        return None;
    }
    let Type::BoundMethod(init) = model
        .member(db, env, "__init__")
        .place
        .ignore_possibly_undefined()?
    else {
        return None;
    };
    let [declared] = function.signature(db).overloads.as_slice() else {
        return None;
    };
    let declared_parameters = declared.parameters();
    declared_parameters.keyword_variadic()?;

    // `create()` only passes keywords on, so positional parameters of `__init__` become
    // keyword-only, and positional-only and variadic positional parameters are dropped.
    let overloads = init.bound_signatures(db)?.iter().map(|init| {
        let keywords = init
            .parameters()
            .iter()
            .filter(|parameter| {
                parameter
                    .keyword_name()
                    .is_none_or(|name| declared_parameters.keyword_by_name(name).is_none())
            })
            .filter_map(|parameter| match parameter.kind() {
                ParameterKind::PositionalOrKeyword { .. } => {
                    Some(parameter.positional_or_keyword_to_keyword_only())
                }
                ParameterKind::KeywordOnly { .. } | ParameterKind::KeywordVariadic { .. } => {
                    Some(parameter.clone())
                }
                ParameterKind::PositionalOnly { .. } | ParameterKind::Variadic { .. } => None,
            });
        let parameters = declared_parameters
            .iter()
            .filter(|parameter| !parameter.is_keyword_variadic())
            .cloned()
            .chain(keywords);
        Signature::new_generic(
            declared.generic_context,
            Parameters::standard(parameters),
            declared.return_ty,
        )
        .with_definition(declared.definition)
    });
    Some(Type::Callable(CallableType::new(
        db,
        CallableSignature::from_overloads(overloads),
        CallableTypeKind::FunctionLike,
    )))
}

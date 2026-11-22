//! Support for Django models, managers and querysets, as typed by django-stubs.
//!
//! The members that Django creates at runtime are synthesized by [`synthesized_member`].

use crate::types::call::{Argument, Binding, CallArguments};
use crate::types::{
    BoundMethodType, ClassBase, KnownClass, Specialization, StaticClassLiteral, Type,
};
use crate::{Db, ProgramEnvironment};

mod fields;
mod managers;
mod model;
mod project;
mod queryset;
mod relations;

pub(in crate::types) use fields::is_renamed_field;
use fields::renamed_field;
use managers::{
    bound_manager, from_queryset_return_type, inherited_manager, manager_queryset_method,
};
pub(in crate::types) use model::is_automatic_primary_key_member;
use model::{
    PrimaryKeyMember, date_navigation_method, display_method, foreign_key_id_member, init_method,
    model_exception, parent_link_member, primary_key_member,
};
use project::setting;
pub(in crate::types) use project::type_alias_override;
use queryset::{annotate_return_type, values_return_type};
use relations::refined_field_type;
use relations::reverse_accessor;
pub(in crate::types) use relations::{foreign_key_model_type, is_foreign_key};

/// Returns whether `class` is the known class `known` or a subclass of it.
fn inherits_from<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>, known: KnownClass) -> bool {
    class
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .any(|base| base.is_known(db, known))
}

/// Returns whether `class` is a subclass of Django's `Model`.
pub(in crate::types) fn is_model<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>) -> bool {
    class
        .iter_mro(db, None)
        .skip(1)
        .filter_map(ClassBase::into_class)
        .any(|base| base.is_known(db, KnownClass::DjangoModel))
}

/// Provides synthesized attributes and methods for Django models.
pub(in crate::types) fn synthesized_member<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<Type<'db>> {
    if class.is_known(db, KnownClass::DjangoBaseManager) {
        return manager_queryset_method(db, env, specialization?, name);
    }
    if let Some(setting) = setting(db, env, class, name) {
        return Some(setting);
    }

    if name == "__init__" && is_model(db, class) {
        return init_method(db, env, class, specialization);
    }
    // Django does not create other members with a private name.
    if name.starts_with('_') || !is_model(db, class) {
        return None;
    }
    // A quick filter on the name of the members that depend on the fields of the model. A reverse
    // accessor can have any name.
    if !(matches!(
        name,
        "id" | "pk" | "DoesNotExist" | "MultipleObjectsReturned"
    ) || name.ends_with("_id")
        || name.ends_with("_ptr")
        || name.starts_with("get_") && name.ends_with("_display")
        || name.starts_with("get_next_by_")
        || name.starts_with("get_previous_by_"))
    {
        return renamed_field(db, class, specialization, name)
            .or_else(|| inherited_manager(db, env, class, name))
            .or_else(|| reverse_accessor(db, class, name));
    }
    match name {
        "DoesNotExist" | "MultipleObjectsReturned" => model_exception(db, env, class, name),
        "id" | "pk" => match primary_key_member(db, env, class, specialization, name)? {
            PrimaryKeyMember::Automatic(ty) | PrimaryKeyMember::Explicit(ty) => Some(ty),
        },
        _ => display_method(db, env, class, specialization, name)
            .or_else(|| date_navigation_method(db, env, class, specialization, name))
            .or_else(|| foreign_key_id_member(db, env, class, specialization, name))
            .or_else(|| parent_link_member(db, env, class, specialization, name))
            .or_else(|| reverse_accessor(db, class, name)),
    }
}

/// Return a more precise type for the member `name` of the class `class`, whose type according to the
/// stubs is `ty`: a relation field whose related model the stubs could not determine, or a manager
/// that is not yet bound to its model.
pub(in crate::types) fn refined_class_member<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    refined_field_type(db, env, class, name, ty).or_else(|| bound_manager(db, env, class, ty))
}

/// Return the type of a call to a method of a Django manager or queryset, for the methods whose
/// result the stubs cannot express: `Manager.from_queryset()`, `annotate()`, `values()` and
/// `values_list()`. `binding` is the matched overload of the call.
///
/// `annotate()`, `values()` and `values_list()` are only refined when they are called on one of
/// Django's managers or querysets, as the django-stubs mypy plugin does. A method with the same
/// name on another class keeps its declared type, even when it returns a `QuerySet`.
pub(in crate::types) fn queryset_method_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    bound_method: BoundMethodType<'db>,
    binding: &Binding<'db>,
    arguments: &CallArguments<'_, 'db>,
) -> Option<Type<'db>> {
    let function = bound_method.function(db)?;
    let receiver = bound_method.self_instance(db);
    let is_django_receiver = || {
        receiver.nominal_class(db, env).is_some_and(|class| {
            class
                .iter_mro(db)
                .filter_map(ClassBase::into_class)
                .any(|base| {
                    base.is_known(db, KnownClass::DjangoQuerySet)
                        || base.is_known(db, KnownClass::DjangoBaseManager)
                })
        })
    };
    match function.name(db).as_str() {
        "from_queryset" => from_queryset_return_type(db, env, receiver, binding),
        "annotate" if is_django_receiver() => annotate_return_type(
            db,
            env,
            binding.return_type(),
            arguments
                .iter()
                .filter_map(|argument| match argument.kind() {
                    Argument::Keyword(name) => Some(name),
                    _ => None,
                }),
        ),
        "values" | "values_list" if is_django_receiver() => {
            values_return_type(db, env, receiver, binding, arguments)
        }
        _ => None,
    }
}

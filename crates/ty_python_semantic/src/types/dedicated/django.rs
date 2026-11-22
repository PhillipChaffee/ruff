//! Support for Django models, managers and querysets, as typed by django-stubs.
//!
//! The members that Django creates at runtime are synthesized by [`synthesized_member`].

use crate::types::{ClassBase, KnownClass, Specialization, StaticClassLiteral, Type};
use crate::{Db, ProgramEnvironment};

mod fields;
mod managers;
mod model;
mod project;
mod queryset;
mod relations;

pub(in crate::types) use fields::is_renamed_field;
use fields::renamed_field;
pub(in crate::types) use managers::from_queryset_return_type;
use managers::manager_queryset_method;
pub(in crate::types) use model::is_automatic_primary_key_member;
use model::{
    PrimaryKeyMember, date_navigation_method, display_method, foreign_key_id_member, init_method,
    model_exception, parent_link_member, primary_key_member,
};
use project::setting;
pub(in crate::types) use project::type_alias_override;
pub(in crate::types) use queryset::{
    QuerySetRows, annotate_return_type, queryset_rows_return_type,
};
use relations::reverse_accessor;
pub(in crate::types) use relations::{foreign_key_model_type, is_foreign_key, refined_field_type};

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

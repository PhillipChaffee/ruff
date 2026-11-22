//! Support for Django models, managers and querysets, as typed by django-stubs.
//!
//! The members that Django creates at runtime are synthesized by [`synthesized_member`].

use ruff_python_ast as ast;
use ty_module_resolver::file_to_module;

use crate::types::call::{Binding, Bindings, CallArguments};
use crate::types::context::InferContext;
use crate::types::function::FunctionType;
use crate::types::{
    BoundMethodType, ClassBase, KnownClass, Specialization, StaticClassLiteral, Type,
};
use crate::{Db, ProgramEnvironment};

mod fields;
mod lookups;
mod managers;
mod model;
mod project;
mod queryset;
mod relations;

pub(in crate::types) use fields::is_renamed_field;
use fields::renamed_field;
use lookups::{FieldNamesMethod, query_context, report_field_names, report_update_fields};
use managers::{
    bound_manager, create_method, from_queryset_return_type, inherited_manager,
    manager_queryset_method, related_manager_member,
};
pub(in crate::types) use model::is_automatic_primary_key_member;
use model::{
    PrimaryKeyMember, date_navigation_method, display_method, foreign_key_id_member, init_method,
    model_exception, parent_link_member, primary_key_member,
};
use project::setting;
pub(in crate::types) use project::type_alias_override;
use queryset::{
    annotate_return_type, prefetch_related_return_type, report_values_list_arguments,
    values_return_type,
};
pub(in crate::types) use queryset::{constructed_instance_type, type_alias_value_override};
use relations::reverse_accessor;
pub(in crate::types) use relations::{foreign_key_model_type, is_foreign_key};
use relations::{get_model_return_type, refined_field_type};

/// Returns whether `class` is the known class `known` or a subclass of it.
fn inherits_from<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>, known: KnownClass) -> bool {
    class
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .any(|base| base.is_known(db, known))
}

/// Returns whether `class` is a subclass of Django's `Model`.
///
/// This is cached, because every member lookup on every class asks it before anything else.
#[salsa::tracked(returns(copy), cycle_initial=|_, _, _| false, heap_size=ruff_memory_usage::heap_size)]
pub(in crate::types) fn is_model<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>) -> bool {
    class
        .iter_mro(db, None)
        .skip(1)
        .filter_map(ClassBase::into_class)
        .any(|base| base.is_known(db, KnownClass::DjangoModel))
}

/// Returns whether Django reads the options of `class` from a nested `Meta` class.
///
/// Models and forms are configured this way, and a subclass routinely replaces the `Meta` class
/// of its base with an unrelated one:
///
/// ```python
/// class BookForm(forms.ModelForm):
///     class Meta:
///         model = Book
///         fields = ["title"]
///
/// class FullBookForm(BookForm):
///     class Meta:
///         model = Book
///         fields = "__all__"
/// ```
pub(in crate::types) fn has_meta_options<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> bool {
    is_model(db, class) || inherits_from(db, class, KnownClass::DjangoBaseForm)
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
    if let Some(member) = related_manager_member(db, env, class, specialization, name) {
        return Some(member);
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

/// Return a more precise type for the member `name` of the class `class`, specialized with
/// `specialization`, whose type according to the stubs is `ty`: a relation field whose related
/// model the stubs could not determine, a manager that is not yet bound to its model, or the
/// `create()` method of a manager or queryset.
pub(in crate::types) fn refined_class_member<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    if matches!(name, "create" | "acreate")
        && let Some(method) = create_method(db, env, class, specialization, ty)
    {
        return Some(method);
    }
    if !is_model(db, class) {
        return None;
    }
    refined_field_type(db, env, class, name, ty).or_else(|| bound_manager(db, env, class, ty))
}

/// Return the type of a call to a method of a Django manager, queryset or app registry, for the
/// methods whose result the stubs cannot express: `Manager.from_queryset()`, `annotate()`,
/// `alias()`, `prefetch_related()`, `values()`, `values_list()` and `Apps.get_model()`. `binding` is the
/// matched overload of the call.
///
/// The methods other than `from_queryset()` are only refined when they are called on one of
/// Django's managers or querysets, as the django-stubs mypy plugin does. A method with the same
/// name on another class keeps its declared type, even when it returns a `QuerySet`.
///
/// `get_model()` is only refined on `Apps` itself. Its subclass `StateApps`, the registry that a
/// migration receives, holds historical models, which can differ from the models in the source
/// code.
pub(in crate::types) fn method_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    bound_method: BoundMethodType<'db>,
    binding: &Binding<'db>,
    arguments: &CallArguments<'_, 'db>,
) -> Option<Type<'db>> {
    let function = bound_method.function(db)?;
    let receiver = bound_method.self_instance(db);
    let is_django_receiver = || is_queryset_or_manager(db, env, receiver);
    match function.name(db).as_str() {
        "from_queryset" => from_queryset_return_type(db, env, receiver, binding),
        "annotate" | "alias" if is_django_receiver() => {
            annotate_return_type(db, env, receiver, binding, arguments)
        }
        "prefetch_related" if is_django_receiver() => {
            prefetch_related_return_type(db, env, receiver, binding, arguments)
        }
        "values" | "values_list" if is_django_receiver() => {
            values_return_type(db, env, receiver, binding, arguments)
        }
        "get_model"
            if receiver
                .nominal_class(db, env)
                .is_some_and(|class| class.is_known(db, KnownClass::DjangoApps)) =>
        {
            get_model_return_type(db, env, binding)
        }
        _ => None,
    }
}

/// Returns whether `ty` is an instance of one of Django's `QuerySet` or manager classes.
fn is_queryset_or_manager<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    ty: Type<'db>,
) -> bool {
    ty.nominal_class(db, env).is_some_and(|class| {
        class
            .iter_mro(db)
            .filter_map(ClassBase::into_class)
            .any(|base| {
                base.is_known(db, KnownClass::DjangoQuerySet)
                    || base.is_known(db, KnownClass::DjangoBaseManager)
            })
    })
}

/// Returns whether Django declares `function`, rather than a class of the project that overrides
/// a method of Django.
fn is_django_function<'db>(db: &'db dyn Db, function: FunctionType<'db>) -> bool {
    file_to_module(db, function.program_file(db).resolver_file(db))
        .is_some_and(|module| module.name(db).first_component() == "django")
}

/// Report the arguments of a call to a Django method that Django rejects at runtime, although the
/// stubs accept their types: the arguments of `values_list()`, and the field names that
/// `order_by()`, `only()`, `defer()`, `select_related()` and `save(update_fields=...)` cannot
/// resolve. `bindings` is the result of binding the arguments of `call_expression`.
///
/// This only checks calls of the methods that Django declares.
pub(in crate::types) fn report_queryset_call<'db>(
    context: &InferContext<'db, '_>,
    call_expression: &ast::ExprCall,
    bindings: &Bindings<'db>,
) {
    let db = context.db();
    let env = context.program_environment();
    let arguments = &call_expression.arguments;
    for binding in bindings.iter_flat() {
        let Type::BoundMethod(bound_method) = binding.callable_type else {
            continue;
        };
        let Some(function) = bound_method.function(db) else {
            continue;
        };
        let name = function.name(db).as_str();
        let field_names_method = FieldNamesMethod::from_name(name);
        if !(field_names_method.is_some() || matches!(name, "values_list" | "save" | "asave"))
            || !is_django_function(db, function)
        {
            continue;
        }
        let receiver = bound_method.self_instance(db);
        if let Some(method) = field_names_method {
            if let Some(query) = query_context(db, env, receiver) {
                report_field_names(context, method, &query, arguments);
            }
        } else if name == "values_list" {
            if is_queryset_or_manager(db, env, receiver)
                && let Some((_, overload)) = binding.matching_overloads().next()
            {
                report_values_list_arguments(context, arguments, overload);
            }
        } else if let Type::NominalInstance(instance) = receiver
            && let Some((model, _)) = instance.class(db, env).static_class_literal(db)
            && is_model(db, model)
        {
            report_update_fields(context, model, arguments);
        }
    }
}

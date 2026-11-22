//! The fields of a model and its `Meta` options, read from the model's class body.

use ruff_db::parsed::parsed_module;
use ruff_python_ast::name::Name;
use ty_python_core::{
    definition::{Definition, DefinitionKind},
    place_table, semantic_index, use_def_map,
};

use crate::place::place_from_bindings;
use crate::types::infer::nearest_enclosing_class;
use crate::types::member::class_member;
use crate::types::{
    ClassBase, KnownClass, Specialization, StaticClassLiteral, Type, binding_type,
    definition_expression_type,
};
use crate::{Db, ProgramEnvironment};

use super::is_model;

/// The type of a Django model field, and metadata extracted from the field construction.
///
/// For example, `isbn` is a `CharField` with `metadata.primary_key` set to `true`.
/// ```py
/// class Book(models.Model):
///     isbn = CharField(max_length=13, primary_key=True)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) struct Field<'db> {
    /// The name of the attribute that Django creates for the field.
    pub(super) name: Name,
    /// The name that the field is assigned to in the class body. It differs from `name` if the
    /// field construction has a `name` argument.
    pub(super) symbol: Name,
    pub(super) ty: Type<'db>,
    /// The type of the first argument of the field construction, which names the related model
    /// of a relation field: `Author` or `"Author"` in `ForeignKey(Author, ...)`.
    pub(super) to: Option<Type<'db>>,
    /// The type of the `through` argument of a `ManyToManyField`, which names the model of the
    /// intermediate table.
    pub(super) through: Option<Type<'db>>,
    pub(super) metadata: FieldMetadata,
}

impl<'db> Field<'db> {
    /// Create a field by extracting the metadata from its definition.
    fn from_definition(
        db: &'db dyn Db,
        name: Name,
        ty: Type<'db>,
        definition: Definition<'db>,
    ) -> Self {
        let mut field = Field {
            symbol: name.clone(),
            name,
            ty,
            to: None,
            through: None,
            metadata: FieldMetadata::default(),
        };
        let module = parsed_module(db, definition.python_file(db)).load(db);
        let kind = definition.kind(db);
        let value = match &kind {
            DefinitionKind::Assignment(assignment) => assignment.value(&module),
            DefinitionKind::AnnotatedAssignment(assignment) => {
                let Some(value) = assignment.value(&module) else {
                    return field;
                };
                value
            }
            _ => return field,
        };
        let Some(call) = value.as_call_expr() else {
            return field;
        };

        field.to = call
            .arguments
            .find_argument_value("to", 0)
            .map(|to| definition_expression_type(db, definition, to));
        field.through = call
            .arguments
            .find_keyword("through")
            .map(|through| definition_expression_type(db, definition, &through.value));

        if let Some(name) = call.arguments.find_keyword("name")
            && let Some(name) =
                definition_expression_type(db, definition, &name.value).as_string_literal()
            && !name.value(db).is_empty()
        {
            field.name = Name::new(name.value(db));
        }

        field.metadata.primary_key =
            call.arguments
                .find_keyword("primary_key")
                .is_some_and(|primary_key| {
                    definition_expression_type(db, definition, &primary_key.value).as_bool_literal()
                        == Some(true)
                });
        field.metadata.has_choices =
            call.arguments
                .find_keyword("choices")
                .is_some_and(|choices| {
                    !definition_expression_type(db, definition, &choices.value).is_none(db)
                });
        field.metadata.null = call.arguments.find_keyword("null").is_some_and(|null| {
            definition_expression_type(db, definition, &null.value).as_bool_literal() == Some(true)
        });
        field.metadata.to_field = call
            .arguments
            .find_keyword("to_field")
            .and_then(|to_field| {
                definition_expression_type(db, definition, &to_field.value)
                    .as_string_literal()
                    .map(|name| Name::new(name.value(db)))
            });
        field.metadata.related_name =
            call.arguments
                .find_keyword("related_name")
                .and_then(|related_name| {
                    definition_expression_type(db, definition, &related_name.value)
                        .as_string_literal()
                        .map(|name| Name::new(name.value(db)))
                });
        field.metadata.parent_link =
            call.arguments
                .find_keyword("parent_link")
                .is_some_and(|parent_link| {
                    definition_expression_type(db, definition, &parent_link.value).as_bool_literal()
                        == Some(true)
                });
        field
    }
}

/// Metadata extracted from a Django field's construction.
#[derive(Debug, Clone, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
#[expect(clippy::struct_excessive_bools)]
pub(super) struct FieldMetadata {
    pub(super) primary_key: bool,
    pub(super) has_choices: bool,
    pub(super) null: bool,
    pub(super) to_field: Option<Name>,
    pub(super) parent_link: bool,
    pub(super) related_name: Option<Name>,
}

/// All fields defined on this Django model (not including base class fields).
#[salsa::tracked(returns(deref), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn own_model_fields<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> Box<[Field<'db>]> {
    let env = ProgramEnvironment::from_scope(class.body_scope(db));
    let table = place_table(db, class.body_scope(db));
    use_def_map(db, class.body_scope(db))
        .all_end_of_scope_symbol_bindings()
        .flat_map(|(symbol_id, bindings)| {
            let name = table.symbol(symbol_id).name();
            bindings.filter_map(move |binding| {
                binding
                    .binding
                    .definition()
                    .map(|definition| (name, definition))
            })
        })
        .filter_map(|(name, definition)| {
            let field_ty = binding_type(db, definition);
            let is_field = field_ty.nominal_class(db, &env).is_some_and(|class| {
                class
                    .iter_mro(db)
                    .filter_map(ClassBase::into_class)
                    .any(|base| base.is_known(db, KnownClass::DjangoField))
            });
            if !is_field {
                return None;
            }
            Some(Field::from_definition(
                db,
                name.clone(),
                field_ty,
                definition,
            ))
        })
        .collect()
}

/// Metadata extracted from a Django model's `Meta` configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) struct ModelMetadata {
    pub(super) is_abstract: bool,
    pub(super) is_proxy: bool,
}

#[salsa::tracked(returns(copy), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn model_metadata<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> ModelMetadata {
    let env = ProgramEnvironment::from_scope(class.body_scope(db));
    // Subclasses do not inherit `abstract = True`, so only inspect this model's own Meta.
    let Some(meta) = own_meta(db, class) else {
        return ModelMetadata::default();
    };
    ModelMetadata {
        is_abstract: is_abstract(db, &env, class, meta),
        // Unlike `abstract`, Django never resets `proxy`.
        is_proxy: meta
            .iter_mro(db, None)
            .filter_map(ClassBase::into_class)
            .map_while(|meta_base| meta_base.static_class_literal(db))
            .find_map(|(meta_base, _)| meta_setting(db, &env, meta_base, "proxy"))
            .unwrap_or(false),
    }
}

/// Returns whether `meta`, the `Meta` configuration of the model `class`, makes it abstract.
pub(super) fn is_abstract<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    meta: StaticClassLiteral<'db>,
) -> bool {
    for meta_base in meta.iter_mro(db, None).filter_map(ClassBase::into_class) {
        let Some((meta_base, _)) = meta_base.static_class_literal(db) else {
            return false;
        };
        // Django resets an abstract model's Meta.abstract to False during model creation.
        // A child can inherit other options from that Meta, but must set abstract=True itself.
        let enclosing_scope = meta_base.definition(db).scope(db);
        if let Some(owner) = nearest_enclosing_class(
            db,
            semantic_index(db, enclosing_scope.program_file(db)),
            enclosing_scope,
        ) && owner != class
            && is_model(db, owner)
            && own_meta(db, owner) == Some(meta_base)
        {
            return false;
        }

        if let Some(is_abstract) = meta_setting(db, env, meta_base, "abstract") {
            return is_abstract;
        }
    }
    false
}

/// Returns whether the body of the `Meta` class `meta` sets `name` to `True`, or `None` if it has
/// no such setting.
fn meta_setting<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    meta: StaticClassLiteral<'db>,
    name: &str,
) -> Option<bool> {
    let scope = meta.body_scope(db);
    let symbol = place_table(db, scope).symbol_id(name)?;
    // The public type is widened to `bool`, so read the bindings' literal values instead.
    let value = place_from_bindings(
        db,
        env,
        use_def_map(db, scope).end_of_scope_symbol_bindings(symbol),
    )
    .place
    .ignore_possibly_undefined()?;
    Some(value == Type::bool_literal(true))
}

fn own_meta<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> Option<StaticClassLiteral<'db>> {
    let meta = class_member(db, class.body_scope(db), "Meta")
        .ignore_possibly_undefined()?
        .as_class_literal()?
        .as_static()?;
    (meta.name(db).as_str() == "Meta" && meta.definition(db).scope(db) == class.body_scope(db))
        .then_some(meta)
}

/// Synthesize the attribute of a field whose `name` argument differs from the name it is assigned
/// to. Django creates the attribute under the name given by the argument:
/// ```py
/// class Article(models.Model):
///     views = models.IntegerField(name="view_count")
///
/// # Article().view_count has type int.
/// ```
pub(super) fn renamed_field<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<Type<'db>> {
    own_model_fields(db, class)
        .iter()
        .find(|field| field.name == name)
        .map(|field| field.ty.apply_optional_specialization(db, specialization))
}

/// Returns whether `name` is the name that a field of the model `class` is assigned to, but not
/// the name of the attribute that Django creates for it. `ty` is the type of the class member.
///
/// Django removes the original attribute of a field whose `name` argument renames it:
/// ```py
/// class Article(models.Model):
///     views = models.IntegerField(name="view_count")
///
/// # Article().views does not exist.
/// ```
pub(in crate::types) fn is_renamed_field<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
    ty: Type<'db>,
) -> bool {
    is_model(db, class)
        && ty.nominal_class(db, env).is_some_and(|field_class| {
            field_class
                .iter_mro(db)
                .filter_map(ClassBase::into_class)
                .any(|base| base.is_known(db, KnownClass::DjangoField))
        })
        && own_model_fields(db, class)
            .iter()
            .any(|field| field.symbol == name && field.name != name)
}

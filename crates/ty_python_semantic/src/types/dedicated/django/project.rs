//! Information that spans the project: its models, its settings, and the django-stubs aliases that
//! depend on them.

use ruff_db::files::File;
use ruff_db::parsed::parsed_module;
use ruff_python_ast::name::Name;
use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{self as ast};
use rustc_hash::FxHashSet;
use ty_module_resolver::{
    ImportingFile, KnownModule, Module, ModuleName, ResolverEnvironment, SearchPath,
    file_to_module, list_modules, resolve_module_confident, resolve_module_for_import_from,
};
use ty_python_core::{ProgramFile, global_scope, use_def_map};

use crate::place::{global_symbol, known_module_symbol};
use crate::types::{
    Parameter, Parameters, Signature, StaticClassLiteral, Type, UnionType, binding_type,
};
use crate::{Db, FxIndexSet, ProgramEnvironment};

use super::is_model;

/// Return the type of the setting `name` on `django.conf.settings`, an instance of `LazySettings`.
///
/// The stubs give every setting the type `Any`, through a `__getattr__` method. We look the setting
/// up in the project's settings module, if we can find it (see [`project_settings`]), and then in
/// `django.conf.global_settings`, where the stubs declare the defaults of Django's own settings
/// with their types, such as `DEBUG: bool`.
///
/// If we read the whole settings module, we also replace `__getattr__`, so that a setting that is
/// defined nowhere is reported. Otherwise, such a setting keeps the type `Any`.
pub(super) fn setting<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
) -> Option<Type<'db>> {
    // Settings are uppercase. Check the names first to avoid a module-resolution query for every
    // class with an unresolved attribute.
    let is_setting_name = name.starts_with(|c: char| c.is_ascii_uppercase())
        && !name.contains(|c: char| c.is_ascii_lowercase());
    if class.name(db) != "LazySettings"
        || !(is_setting_name || name == "__getattr__")
        || !file_to_module(db, class.body_scope(db).program_file(db).resolver_file(db))
            .is_some_and(|module| module.is_known(db, KnownModule::DjangoConf))
    {
        return None;
    }
    let project_settings = project_settings(db, class.body_scope(db).program_file(db));

    if name == "__getattr__" {
        // Every setting that exists is a synthesized member, so `__getattr__` is only reached for
        // a name that is defined nowhere. Its parameter type `Never` rejects every name.
        let project_settings = project_settings?;
        if !project_settings.is_complete {
            return None;
        }
        let self_ty = Type::instance(db, env, class.default_specialization(db));
        let signature = Signature::new(
            Parameters::standard([
                Parameter::positional_or_keyword(Name::new_static("self"))
                    .with_annotated_type(self_ty),
                Parameter::positional_or_keyword(Name::new_static("name"))
                    .with_annotated_type(Type::Never),
            ]),
            Type::any(),
        );
        return Some(Type::function_like_callable(db, signature));
    }

    project_settings
        .and_then(|project_settings| {
            // A setting usually has a different value in each environment, as in `DEBUG = True`
            // for development, so we promote a literal value such as `True` to `bool`.
            global_symbol(db, project_settings.file, name)
                .place
                .ignore_possibly_undefined()
                .map(|ty| ty.promote(db, env))
        })
        .or_else(|| {
            known_module_symbol(db, env, KnownModule::DjangoConfGlobalSettings, name)
                .place
                .ignore_possibly_undefined()
        })
}

/// The settings module of the project, found without running any code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
struct ProjectSettings<'db> {
    file: ProgramFile<'db>,
    /// Whether we can see every setting that the module defines. A module that star-imports a
    /// module that we cannot resolve, for example, can define settings that we do not see.
    is_complete: bool,
}

/// Find the settings module of the project, for the program of `conf`, the file of
/// `django.conf`.
///
/// NOTE FOR REVIEWERS: this reads the project's own settings module, which no other part of ty's
/// Django support does. It runs no code. Django reads the module named by the
/// `DJANGO_SETTINGS_MODULE` environment variable. We do not read the environment of the process
/// that runs ty, because it is usually not the environment that runs Django. Instead, we use the
/// default that a project's `manage.py` sets, as `django-admin startproject` generates it:
/// ```py
/// os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
/// ```
///
/// We only use a first-party `manage` module. A project that sets `DJANGO_SETTINGS_MODULE`
/// elsewhere, or that runs with a different settings module than its `manage.py` names, can get
/// the wrong settings, which can report settings that the real settings module defines.
#[salsa::tracked(returns(copy), heap_size=ruff_memory_usage::heap_size)]
fn project_settings<'db>(db: &'db dyn Db, conf: ProgramFile<'db>) -> Option<ProjectSettings<'db>> {
    let env = ProgramEnvironment::from_file(conf);
    let resolver_environment = env.resolver_environment(db);
    let first_party_file = |name: &ModuleName| {
        let module = resolve_module_confident(db, resolver_environment, name)?;
        if !module
            .search_path(db)
            .is_some_and(SearchPath::is_first_party)
        {
            return None;
        }
        module.file(db)
    };

    let manage = first_party_file(&ModuleName::new_static("manage")?)?;
    let mut finder = SettingsModuleFinder::default();
    let program_file = |file| ProgramFile::new(db, file, env.program(db));
    finder.visit_body(
        parsed_module(db, program_file(manage).python_file(db))
            .load(db)
            .suite(),
    );
    let file = first_party_file(&ModuleName::new(&finder.settings_module?)?)?;

    // Follow the star imports of the settings module, which commonly splits its settings over
    // several modules with `from .base import *`.
    let mut visited = FxHashSet::default();
    let mut pending = vec![file];
    let mut is_complete = true;
    while let Some(file) = pending.pop() {
        if !visited.insert(file) {
            continue;
        }
        let mut checker = CompletenessChecker {
            db,
            importing_file: ImportingFile::File(file, resolver_environment),
            star_imports: Vec::new(),
            is_complete: true,
        };
        checker.visit_body(
            parsed_module(db, program_file(file).python_file(db))
                .load(db)
                .suite(),
        );
        is_complete &= checker.is_complete;
        pending.extend(checker.star_imports);
    }

    Some(ProjectSettings {
        file: program_file(file),
        is_complete,
    })
}

/// Finds the module name in `os.environ.setdefault("DJANGO_SETTINGS_MODULE", "<module>")`.
#[derive(Default)]
struct SettingsModuleFinder {
    settings_module: Option<String>,
}

impl<'ast> Visitor<'ast> for SettingsModuleFinder {
    fn visit_expr(&mut self, expr: &'ast ast::Expr) {
        if let ast::Expr::Call(call) = expr
            && let ast::Expr::Attribute(attribute) = &*call.func
            && attribute.attr.as_str() == "setdefault"
            && let [variable, value] = &*call.arguments.args
            && let Some(variable) = variable.as_string_literal_expr()
            && variable.value.to_str() == "DJANGO_SETTINGS_MODULE"
            && let Some(value) = value.as_string_literal_expr()
        {
            self.settings_module
                .get_or_insert_with(|| value.value.to_str().to_owned());
        }
        walk_expr(self, expr);
    }
}

/// Checks whether we can see every setting that a settings module defines, and collects the
/// modules that it star-imports.
///
/// A module can define names that we cannot see through a star import that we cannot resolve,
/// such as `from .local import *` for a file that is not checked in; through `globals()`,
/// `vars()`, `locals()` or `exec()`; through a module-level `__getattr__`; or through
/// `django-split-settings`, which executes other files.
struct CompletenessChecker<'db> {
    db: &'db dyn Db,
    importing_file: ImportingFile<'db>,
    star_imports: Vec<File>,
    is_complete: bool,
}

impl<'ast> Visitor<'ast> for CompletenessChecker<'_> {
    fn visit_stmt(&mut self, stmt: &'ast ast::Stmt) {
        match stmt {
            ast::Stmt::ImportFrom(import)
                if import
                    .module
                    .as_ref()
                    .is_some_and(|module| module.as_str().starts_with("split_settings")) =>
            {
                self.is_complete = false;
            }
            ast::Stmt::ImportFrom(import)
                if import.names.iter().any(|alias| &alias.name == "*") =>
            {
                match resolve_module_for_import_from(self.db, self.importing_file, import)
                    .and_then(|module| module.file(self.db))
                {
                    Some(file) => self.star_imports.push(file),
                    None => self.is_complete = false,
                }
            }
            ast::Stmt::Import(import)
                if import
                    .names
                    .iter()
                    .any(|alias| alias.name.as_str().starts_with("split_settings")) =>
            {
                self.is_complete = false;
            }
            ast::Stmt::FunctionDef(function) if function.name.as_str() == "__getattr__" => {
                self.is_complete = false;
            }
            _ => {}
        }
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'ast ast::Expr) {
        if let ast::Expr::Call(call) = expr
            && let ast::Expr::Name(name) = &*call.func
            && matches!(name.id.as_str(), "globals" | "vars" | "locals" | "exec")
        {
            self.is_complete = false;
        }
        walk_expr(self, expr);
    }
}

/// The models that Django knows about when it uses a model defined in `file`: the models in the
/// global scope of `file` itself and of the project's `models` modules (see [`models_modules`]).
#[salsa::tracked(returns(deref), cycle_initial=|_, _, _| Box::default(), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn project_models<'db>(
    db: &'db dyn Db,
    file: ProgramFile<'db>,
) -> Box<[StaticClassLiteral<'db>]> {
    let env = ProgramEnvironment::from_file(file);
    // A module that several `models` modules import from contributes each model only once.
    let models: FxIndexSet<_> = std::iter::once(file)
        .chain(
            models_modules(db, env.resolver_environment(db))
                .iter()
                .map(|module| ProgramFile::new(db, *module, env.program(db))),
        )
        .flat_map(|file| {
            use_def_map(db, global_scope(db, file))
                .all_end_of_scope_symbol_bindings()
                .flat_map(|(_, bindings)| {
                    bindings.filter_map(|binding| binding.binding.definition())
                })
        })
        .filter_map(|definition| binding_type(db, definition).as_class_literal()?.as_static())
        .filter(|class| is_model(db, *class))
        .collect();
    models.into_iter().collect()
}

/// The files of the project's `models` modules, including the submodules of a `models` package.
///
/// Django imports the `models` module of each installed app when it starts, and a model only
/// takes part in relations once its module is imported. We do not read the project's settings to
/// find the installed apps, so we treat every first-party module with a `models` component in its
/// name as the models of an app: `library.models` and `library.models.book`, for example.
#[salsa::tracked(returns(deref), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn models_modules<'db>(
    db: &'db dyn Db,
    resolver_environment: ResolverEnvironment<'db>,
) -> Box<[File]> {
    let mut files = Vec::new();
    let mut pending: Vec<Module<'db>> = list_modules(db, resolver_environment)
        .iter()
        .copied()
        .filter(|module| {
            module
                .search_path(db)
                .is_some_and(SearchPath::is_first_party)
        })
        .collect();
    while let Some(module) = pending.pop() {
        if module
            .name(db)
            .components()
            .any(|component| component == "models")
            && let Some(file) = module.file(db)
        {
            files.push(file);
        }
        pending.extend(module.all_submodules(db));
    }
    files.sort_unstable();
    files.into_boxed_slice()
}

/// Return the type that one of django-stubs' type aliases denotes, if we replace it.
///
/// `ty` is the type that the alias denotes according to the stubs.
///
/// `_User` in `django.contrib.auth.models` is the stubs' placeholder for the model named by the
/// `AUTH_USER_MODEL` setting. It is declared as `AbstractBaseUser` and used wherever Django
/// returns the user model, such as `get_user_model()` and `HttpRequest.user`. The stubs expect a
/// type checker to refine it. We do not read settings, so we use `AbstractUser`: it is the base
/// class of Django's default `User` model and of most custom user models, and it has the
/// attributes that `AbstractBaseUser` lacks, such as `username` and `is_superuser`.
///
/// `_ChoicesPrimitive` in `django.utils.choices` is the type of the `choices` argument of a
/// field. Its `type[Choices]` element is currently lost when we simplify the union (see
/// <https://github.com/astral-sh/ty/issues/4551>), which rejects `choices=SomeChoicesClass`. We
/// add the metaclass `ChoicesType`, whose instances are the same class objects.
pub(in crate::types) fn type_alias_override<'db>(
    db: &'db dyn Db,
    file: ProgramFile<'db>,
    name: &str,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    // Check the name first to avoid a module-resolution query for every type alias.
    let module = match name {
        "_User" => KnownModule::DjangoContribAuthModels,
        "_ChoicesPrimitive" => KnownModule::DjangoUtilsChoices,
        _ => return None,
    };
    if !file_to_module(db, file.resolver_file(db))
        .is_some_and(|file_module| file_module.is_known(db, module))
    {
        return None;
    }

    let env = ProgramEnvironment::from_file(file);
    let instance_of = |module, class| {
        known_module_symbol(db, &env, module, class)
            .place
            .ignore_possibly_undefined()?
            .to_instance_approximation(db, &env)
    };
    match module {
        KnownModule::DjangoContribAuthModels => instance_of(module, "AbstractUser"),
        _ => Some(UnionType::from_two_elements(
            db,
            &env,
            ty,
            instance_of(KnownModule::DjangoDbModelsEnums, "ChoicesType")?,
        )),
    }
}

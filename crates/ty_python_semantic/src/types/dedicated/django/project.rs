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
    resolve_real_module_confident,
};
use ty_python_core::definition::DefinitionKind;
use ty_python_core::{Program, ProgramFile, global_scope, place_table, use_def_map};

use crate::place::{DefinedPlace, Definedness, Place, global_symbol, known_module_symbol};
use crate::types::{
    ClassBase, KnownClass, Parameter, Parameters, Signature, StaticClassLiteral, Type, UnionType,
    binding_type,
};
use crate::{Db, FxIndexSet, ProgramEnvironment};

use super::fields::class_body_value;
use super::is_model;
use super::relations::registered_model;

/// Returns whether modules found on `search_path` are project code.
///
/// Besides the project root, this includes the extra search paths: a project can keep its apps in
/// a directory that is not the root, such as `src/`, and put that directory on `PYTHONPATH` or
/// in the `extra-paths` setting. Django finds its apps through `INSTALLED_APPS` wherever they
/// live, so we treat that code like the rest of the project.
pub(super) fn is_project_search_path(search_path: &SearchPath) -> bool {
    search_path.is_first_party() || search_path.is_extra()
}

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
    // Settings are uppercase. Check the name first, because this runs for every class with an
    // unresolved attribute.
    let is_setting_name = name.starts_with(|c: char| c.is_ascii_uppercase())
        && !name.contains(|c: char| c.is_ascii_lowercase());
    if !(is_setting_name || name == "__getattr__")
        || !class.is_known(db, KnownClass::DjangoLazySettings)
    {
        return None;
    }
    let project_settings = project_settings(db, class.body_scope(db).program_file(db).program(db));

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

/// Find the settings module of the project of `program`.
///
/// NOTE FOR REVIEWERS: this reads the project's own settings module, which no other part of ty's
/// Django support does. It runs no code. The settings give the types of `django.conf.settings`
/// (see [`setting`]) and the user model (see [`user_model_setting`]). Django reads the module
/// named by the `DJANGO_SETTINGS_MODULE` environment variable. We do not read the environment of
/// the process that runs ty, because it is usually not the environment that runs Django. Instead,
/// we use the default that a project's `manage.py` sets, as `django-admin startproject` generates
/// it:
/// ```py
/// os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
/// ```
///
/// We only use a `manage` module in the project (see [`is_project_search_path`]). A project that
/// sets `DJANGO_SETTINGS_MODULE` elsewhere, or that runs with a different settings module than its
/// `manage.py` names, can get the wrong settings, which can report settings that the real settings
/// module defines.
#[salsa::tracked(returns(copy), heap_size=ruff_memory_usage::heap_size)]
fn project_settings<'db>(db: &'db dyn Db, program: Program<'db>) -> Option<ProjectSettings<'db>> {
    let env = ProgramEnvironment::from_program(program);
    let resolver_environment = env.resolver_environment(db);
    let project_file = |name: &ModuleName| {
        let module = resolve_module_confident(db, resolver_environment, name)?;
        if !module.search_path(db).is_some_and(is_project_search_path) {
            return None;
        }
        module.file(db)
    };

    let manage = project_file(&ModuleName::new_static("manage")?)?;
    let mut finder = SettingsModuleFinder::default();
    let program_file = |file| ProgramFile::new(db, file, program);
    finder.visit_body(
        parsed_module(db, program_file(manage).python_file(db))
            .load(db)
            .suite(),
    );
    let file = project_file(&ModuleName::new(&finder.settings_module?)?)?;

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
/// A module can define names that we cannot see:
/// - through a star import that we cannot resolve, such as `from .local import *` for a file that
///   is not checked in;
/// - through `globals()`, `vars()`, `locals()`, `exec()`, `setattr()` or `sys.modules`, including
///   under another name, as in `g = globals`;
/// - through a module-level `__getattr__`;
/// - through `django-split-settings` (the `split_settings` package), which executes other files.
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
                    .is_some_and(|module| is_split_settings(module.as_str())) =>
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
                    .any(|alias| is_split_settings(alias.name.as_str())) =>
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
        // Any mention counts, called or not, so that an alias such as `g = globals` and an
        // attribute such as `builtins.globals` or `sys.modules` are caught as well.
        let name = match expr {
            ast::Expr::Name(name) => Some(name.id.as_str()),
            ast::Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
            _ => None,
        };
        if name.is_some_and(|name| {
            matches!(
                name,
                "globals" | "vars" | "locals" | "exec" | "setattr" | "modules"
            )
        }) {
            self.is_complete = false;
        }
        walk_expr(self, expr);
    }
}

/// Returns whether `module` is the `split_settings` package of `django-split-settings`, or one of
/// its submodules.
fn is_split_settings(module: &str) -> bool {
    module == "split_settings" || module.starts_with("split_settings.")
}

/// The models that Django knows about when it uses a model defined in `file`: the models in the
/// global scope of `file` itself and of the project's `models` modules (see [`models_modules`]).
///
/// The models of `file` come first, followed by the models of the `models` modules in the order of
/// those modules, each model once.
pub(super) fn project_models<'db>(
    db: &'db dyn Db,
    file: ProgramFile<'db>,
) -> impl Iterator<Item = StaticClassLiteral<'db>> + 'db {
    let own_models = file_models(db, file);
    own_models.iter().copied().chain(
        models_module_models(db, file.program(db))
            .iter()
            .copied()
            .filter(move |model| !own_models.contains(model)),
    )
}

/// The models whose relation fields can refer to a model defined in `file`: the models of
/// [`project_models`], followed by the models of the installed apps outside the project (see
/// [`installed_models`]).
///
/// A model of an installed app is the class from the app's source, whose fields have the arguments
/// that the stubs leave out. Its relations are attached to the class that imports resolve to (see
/// [`imported_model`]). An installed model whose imported class is defined in `file` is left out:
/// the stubs of `file` already declare its relations, as for the models of `django.contrib.auth`
/// when `file` is their stub.
pub(super) fn relation_source_models<'db>(
    db: &'db dyn Db,
    file: ProgramFile<'db>,
) -> impl Iterator<Item = StaticClassLiteral<'db>> + 'db {
    let own_models = file_models(db, file);
    project_models(db, file).chain(
        installed_models(db, file.program(db))
            .iter()
            .filter(move |installed| {
                !own_models.contains(&installed.model)
                    && installed.imported.is_some_and(|imported| {
                        imported == installed.model
                            || imported.body_scope(db).program_file(db) != file
                    })
            })
            .map(|installed| installed.model),
    )
}

/// The models of the project's `models` modules, each model once. A module that several `models`
/// modules import from contributes its models only once.
#[salsa::tracked(returns(deref), cycle_initial=|_, _, _| Box::default(), heap_size=ruff_memory_usage::heap_size)]
fn models_module_models<'db>(
    db: &'db dyn Db,
    program: Program<'db>,
) -> Box<[StaticClassLiteral<'db>]> {
    let env = ProgramEnvironment::from_program(program);
    let models: FxIndexSet<_> = models_modules(db, env.resolver_environment(db))
        .iter()
        .flat_map(|module| {
            file_models(db, ProgramFile::new(db, *module, program))
                .iter()
                .copied()
        })
        .collect();
    models.into_iter().collect()
}

/// A model of an installed app outside the project, read from the app's source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) struct InstalledModel<'db> {
    /// The class from the app's source.
    model: StaticClassLiteral<'db>,
    /// The class that imports of the model resolve to, or `None` if the module that imports
    /// resolve to has no model of that name.
    imported: Option<StaticClassLiteral<'db>>,
}

/// The models of the `models` modules of the installed apps outside the project (see
/// [`installed_app_models_modules`]), each model once, with the classes that imports of them
/// resolve to.
///
/// Code imports a model from the `models` module of its app, so we look the model up by name in
/// the module that imports of `<app>.models` resolve to. This also finds the class from the stubs
/// when the app's source defines the model in a submodule that `models/__init__.py` imports from,
/// and the stubs define it in `models.pyi`.
#[salsa::tracked(returns(deref), cycle_initial=|_, _, _| Box::default(), heap_size=ruff_memory_usage::heap_size)]
fn installed_models<'db>(db: &'db dyn Db, program: Program<'db>) -> Box<[InstalledModel<'db>]> {
    let env = ProgramEnvironment::from_program(program);
    let resolver_environment = env.resolver_environment(db);
    let mut models: Vec<InstalledModel<'db>> = Vec::new();
    for file in installed_app_models_modules(db, program) {
        let program_file = ProgramFile::new(db, *file, program);
        let imported_file = file_to_module(db, program_file.resolver_file(db))
            .and_then(|module| resolve_module_confident(db, resolver_environment, module.name(db)))
            .and_then(|module| module.file(db))
            .map(|imported| ProgramFile::new(db, imported, program));
        for model in file_models(db, program_file) {
            if models.iter().any(|installed| installed.model == *model) {
                continue;
            }
            let imported = match imported_file {
                Some(imported_file) if imported_file != program_file => {
                    model_named(db, imported_file, model.name(db))
                        .or_else(|| imported_from_defining_module(db, *model))
                }
                _ => Some(*model),
            };
            models.push(InstalledModel {
                model: *model,
                imported,
            });
        }
    }
    models.into_boxed_slice()
}

/// Return the class that an import of the model `model` resolves to.
///
/// This differs from `model` for a model of an installed app that we read from its source (see
/// [`installed_models`]) when the app has stubs. Code that imports the model gets the class from
/// the stubs, so relations from or to `model` use that class:
/// ```py
/// from rest_framework.authtoken.models import Token  # the class from the stubs
/// ```
///
/// Returns `None` if the stubs have no model of that name.
#[salsa::tracked(returns(copy), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn imported_model<'db>(
    db: &'db dyn Db,
    model: StaticClassLiteral<'db>,
) -> Option<StaticClassLiteral<'db>> {
    let program = model.body_scope(db).program_file(db).program(db);
    match installed_models(db, program)
        .iter()
        .find(|installed| installed.model == model)
    {
        Some(installed) => installed.imported,
        None => imported_from_defining_module(db, model),
    }
}

/// Return the class of the same name as `model` in the module that imports of the module that
/// defines `model` resolve to.
fn imported_from_defining_module<'db>(
    db: &'db dyn Db,
    model: StaticClassLiteral<'db>,
) -> Option<StaticClassLiteral<'db>> {
    let file = model.body_scope(db).program_file(db);
    let env = ProgramEnvironment::from_file(file);
    let Some(imported) = file_to_module(db, file.resolver_file(db))
        .and_then(|module| {
            resolve_module_confident(db, env.resolver_environment(db), module.name(db))
        })
        .and_then(|module| module.file(db))
        .filter(|imported| *imported != file.file(db))
    else {
        return Some(model);
    };
    model_named(
        db,
        ProgramFile::new(db, imported, file.program(db)),
        model.name(db),
    )
}

/// Return the model that the global scope of `file` binds to `name`.
fn model_named<'db>(
    db: &'db dyn Db,
    file: ProgramFile<'db>,
    name: &str,
) -> Option<StaticClassLiteral<'db>> {
    let class = global_symbol(db, file, name)
        .place
        .ignore_possibly_undefined()?
        .as_class_literal()?
        .as_static()?;
    is_model(db, class).then_some(class)
}

/// The models in the global scope of `file`, including models that it imports.
#[salsa::tracked(returns(deref), cycle_initial=|_, _, _| Box::default(), heap_size=ruff_memory_usage::heap_size)]
fn file_models<'db>(db: &'db dyn Db, file: ProgramFile<'db>) -> Box<[StaticClassLiteral<'db>]> {
    let models: FxIndexSet<_> = use_def_map(db, global_scope(db, file))
        .all_end_of_scope_symbol_bindings()
        .flat_map(|(_, bindings)| bindings.filter_map(|binding| binding.binding.definition()))
        .filter_map(|definition| binding_type(db, definition).as_class_literal()?.as_static())
        .filter(|class| is_model(db, *class))
        .collect();
    models.into_iter().collect()
}

/// The files of the project's `models` modules, including the submodules of a `models` package.
///
/// Django imports the `models` module of each installed app when it starts, and a model only
/// takes part in relations once its module is imported. We do not read the project's settings to
/// find the installed apps, so we treat every project module with a `models` component in its
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
        .filter(|module| module.search_path(db).is_some_and(is_project_search_path))
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
    // A directory that is both under the project root and an extra search path is scanned through
    // both, which reaches the same file as two modules (`app.models` and `src.app.models`).
    files.sort_unstable();
    files.dedup();
    files.into_boxed_slice()
}

/// The files of the `models` modules of the installed apps outside the project, read from their
/// source rather than from their stubs.
///
/// The models of an installed app can have relations to the project's models, usually to the user
/// model. Stubs declare such fields without their arguments, so they do not say which model a
/// field refers to:
/// ```py
/// # rest_framework/authtoken/models.py
/// class Token(models.Model):
///     user = models.OneToOneField(
///         settings.AUTH_USER_MODEL, related_name="auth_token", on_delete=models.CASCADE
///     )
///
/// # rest_framework-stubs/authtoken/models.pyi
/// class Token(models.Model):
///     user: models.OneToOneField
/// ```
///
/// We find the apps in the `INSTALLED_APPS` setting (see [`installed_apps`]). An entry names the
/// package of an app, or an `AppConfig` class whose `name` attribute names the package. The models
/// of an app are in the `models` module of its package. The project's own apps are already part of
/// [`models_modules`].
#[salsa::tracked(returns(deref), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn installed_app_models_modules<'db>(
    db: &'db dyn Db,
    program: Program<'db>,
) -> Box<[File]> {
    let Some(apps) = installed_apps(db, program) else {
        return Box::default();
    };
    let env = ProgramEnvironment::from_program(program);
    let resolver_environment = env.resolver_environment(db);
    let real_module = |name: &str| {
        resolve_real_module_confident(db, resolver_environment, &ModuleName::new(name)?)
    };

    let mut files: Vec<File> = apps
        .iter()
        .filter_map(|entry| {
            let (module, package) = match real_module(entry) {
                Some(module) => (module, entry.clone()),
                // An entry such as `"django.contrib.admin.apps.AdminConfig"`.
                None => {
                    let (module_name, class_name) = entry.rsplit_once('.')?;
                    let module = real_module(module_name)?;
                    let config = global_symbol(
                        db,
                        ProgramFile::new(db, module.file(db)?, program),
                        class_name,
                    )
                    .place
                    .ignore_possibly_undefined()?
                    .as_class_literal()?
                    .as_static()?;
                    // The class can inherit `name`, as Django's `AdminConfig` does from
                    // `SimpleAdminConfig`.
                    let package = config
                        .iter_mro(db, None)
                        .filter_map(ClassBase::into_class)
                        .filter_map(|base| base.static_class_literal(db))
                        .find_map(|(base, _)| class_body_value(db, &env, base, "name"))?
                        .as_string_literal()?
                        .value(db)
                        .to_owned();
                    (module, package)
                }
            };
            if module.search_path(db).is_some_and(is_project_search_path) {
                return None;
            }
            real_module(&format!("{package}.models"))?.file(db)
        })
        .collect();
    files.sort_unstable();
    files.dedup();
    files.into_boxed_slice()
}

/// Return the entries of the `INSTALLED_APPS` setting of the project of `program`, if we can
/// determine them without running any code.
///
/// The settings module, or a module that it star-imports, must always assign a list or tuple of
/// string literals, a sum of such values, or names bound once to them (see
/// [`expression_strings`]), and we must see every setting that it defines (see
/// [`project_settings`]):
/// ```py
/// DJANGO_APPS = ["django.contrib.auth"]
/// INSTALLED_APPS = DJANGO_APPS + ["library"]
/// ```
fn installed_apps<'db>(db: &'db dyn Db, program: Program<'db>) -> Option<Vec<String>> {
    let project_settings = project_settings(db, program)?;
    if !project_settings.is_complete
        || !matches!(
            global_symbol(db, project_settings.file, "INSTALLED_APPS").place,
            Place::Defined(DefinedPlace {
                definedness: Definedness::AlwaysDefined,
                ..
            })
        )
    {
        return None;
    }
    assigned_strings(db, project_settings.file, "INSTALLED_APPS", 0)
}

/// The deepest chain of names that [`assigned_strings`] follows, as in `INSTALLED_APPS = APPS`
/// with `APPS = DJANGO_APPS + LOCAL_APPS`. It also stops names that refer to each other.
const MAX_SETTINGS_NAME_DEPTH: usize = 8;

/// Return the strings of the list or tuple that the global scope of `file` assigns to `name`, if
/// it binds `name` once. A star import is followed to the module that it imports from.
fn assigned_strings<'db>(
    db: &'db dyn Db,
    mut file: ProgramFile<'db>,
    name: &str,
    depth: usize,
) -> Option<Vec<String>> {
    if depth > MAX_SETTINGS_NAME_DEPTH {
        return None;
    }
    let program = file.program(db);
    let resolver_environment = ProgramEnvironment::from_program(program).resolver_environment(db);
    let mut visited = FxHashSet::default();
    while visited.insert(file) {
        let scope = global_scope(db, file);
        let symbol = place_table(db, scope).symbol_id(name)?;
        let mut definitions = use_def_map(db, scope)
            .end_of_scope_symbol_bindings(symbol)
            .filter_map(|binding| binding.binding.definition());
        let (Some(definition), None) = (definitions.next(), definitions.next()) else {
            return None;
        };
        let module = parsed_module(db, definition.python_file(db)).load(db);
        let value = match definition.kind(db) {
            DefinitionKind::Assignment(assignment) if assignment.target(&module).is_name_expr() => {
                assignment.value(&module)
            }
            DefinitionKind::AnnotatedAssignment(assignment) => assignment.value(&module)?,
            // `from .base import *`: look in the module that defines the setting.
            DefinitionKind::StarImport(import) => {
                let imported = resolve_module_for_import_from(
                    db,
                    ImportingFile::File(file.file(db), resolver_environment),
                    import.import(&module),
                )?
                .file(db)?;
                file = ProgramFile::new(db, imported, program);
                continue;
            }
            _ => return None,
        };
        return expression_strings(db, file, value, depth);
    }
    None
}

/// Return the strings of `expr` in the global scope of `file`: a list or tuple of string
/// literals, a name that the module binds once to such a value, or a sum of such values, as in
/// `INSTALLED_APPS = DJANGO_APPS + ["library"]`.
fn expression_strings<'db>(
    db: &'db dyn Db,
    file: ProgramFile<'db>,
    expr: &ast::Expr,
    depth: usize,
) -> Option<Vec<String>> {
    match expr {
        ast::Expr::List(ast::ExprList { elts, .. })
        | ast::Expr::Tuple(ast::ExprTuple { elts, .. }) => elts
            .iter()
            .map(|element| Some(element.as_string_literal_expr()?.value.to_str().to_owned()))
            .collect(),
        ast::Expr::BinOp(ast::ExprBinOp {
            left,
            op: ast::Operator::Add,
            right,
            ..
        }) => {
            let mut strings = expression_strings(db, file, left, depth)?;
            strings.extend(expression_strings(db, file, right, depth)?);
            Some(strings)
        }
        ast::Expr::Name(name) => assigned_strings(db, file, name.id.as_str(), depth + 1),
        _ => None,
    }
}

/// Return the instance type of the user model that the `AUTH_USER_MODEL` setting of the project
/// names (see [`user_model_setting`]), if we can determine it.
pub(super) fn user_model<'db>(db: &'db dyn Db, env: &ProgramEnvironment<'db>) -> Option<Type<'db>> {
    match user_model_setting(db, env.program(db))? {
        "auth.User" => known_module_symbol(db, env, KnownModule::DjangoContribAuthModels, "User")
            .place
            .ignore_possibly_undefined()?
            .to_instance_approximation(db, env),
        reference => registered_model(db, env, reference),
    }
}

/// Return the value of the `AUTH_USER_MODEL` setting of the project of `program`, which names the
/// user model as `"<app label>.<model>"`, if we can determine it without running any code.
///
/// The value must be a string literal that the settings module always assigns:
/// ```py
/// AUTH_USER_MODEL = "accounts.Member"
/// ```
///
/// Without such an assignment, the value is Django's default, `"auth.User"`. We only use the
/// settings module if we can see every setting that it defines (see [`project_settings`]), because
/// it could otherwise set `AUTH_USER_MODEL` in a way that we do not see.
fn user_model_setting<'db>(db: &'db dyn Db, program: Program<'db>) -> Option<&'db str> {
    let project_settings = project_settings(db, program)?;
    if !project_settings.is_complete {
        return None;
    }
    match global_symbol(db, project_settings.file, "AUTH_USER_MODEL").place {
        Place::Undefined => Some("auth.User"),
        Place::Defined(DefinedPlace {
            ty,
            definedness: Definedness::AlwaysDefined,
            ..
        }) => Some(ty.as_string_literal()?.value(db)),
        Place::Defined(_) => None,
    }
}

/// Return the type that one of django-stubs' type aliases denotes, if we replace it.
///
/// `ty` is the type that the alias denotes according to the stubs.
///
/// `_User` in `django.contrib.auth.models` is the stubs' placeholder for the model named by the
/// `AUTH_USER_MODEL` setting. It is declared as `AbstractBaseUser` and used wherever Django
/// returns the user model, such as `get_user_model()` and `HttpRequest.user`. The stubs expect a
/// type checker to refine it. We use the model that the project's settings name (see
/// [`user_model_setting`]). When we cannot determine that model, we use `AbstractUser`: it is the
/// base class of Django's default `User` model and of most custom user models, and it has the
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
        KnownModule::DjangoContribAuthModels => {
            user_model(db, &env).or_else(|| instance_of(module, "AbstractUser"))
        }
        _ => Some(UnionType::from_two_elements(
            db,
            &env,
            ty,
            instance_of(KnownModule::DjangoDbModelsEnums, "ChoicesType")?,
        )),
    }
}

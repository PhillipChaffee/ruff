# Django Support Quality Audit & Holistic Refactoring Findings

> **Temporary branch document:** This file records the post-Round-3 real-project evaluation and
> full code-quality audit of `phillip/django-stubs-switch` at `c559bc8cee` (89 commits,
> `cd2d973241..c559bc8cee`). Remove this document before opening upstream pull requests.
>
> All code examples below use invented models (`Book`, `Author`, `Member`, `Shelf`, `Article`,
> `Review`, `Profile`, `Asset`, `RegionField`, `ActiveQuerySet`, `ListField`). Diagnostic counts
> come from running `ty` and `mypy` against a private 1,558-file Django 5.1 project.

## Executive Summary

Across 89 incremental commits (28 Rust files, +5,794 / −463 lines, 14 `django::` hooks across 10
core files), `ty`'s Django support reached high accuracy on a large real Django codebase:

- **Zero regressions in Round 3 (`2542f029` → `c559bc8c`):** 0 panics, identical output in root
    and `--extra-search-path` modes, 100% of the 164 annotated `null=True` field errors now matched
    with `mypy`, and 4 `F(...)` query-expression false positives fixed.
- **High overall precision:** 98.3% of `ty`-only diagnostics (2,470 / 2,512) are real code errors,
    cascades from invalid annotations, third-party stub/library bugs, or intentional strictness;
    96.1% of `mypy`-only diagnostics (4,516 / 4,699) are `mypy` limitations or intentional `ty`
    design differences.

However, a deep architectural review shows that **nearly every problem solved individually across
those 89 commits — plus all 3 remaining Django gaps observed on the real project, plus 15+ latent
bugs currently in `c559bc8c` — are symptoms of 5 missing holistic abstractions**:

1. **Generic Base Rebinding + Single Field-Type Pipeline** (collapses 5 disjoint field stages and
    3 core constructor hooks into 1; fixes `JSONField(null=True)`, third-party `*args, **kwargs`
    fields, `contrib.postgres`/`gis`, custom `Field` subclasses, and 6 latent bugs).
1. **Algebraic `QuerySetType` / `RowShape` Codec + Intersection-Aware Unwrapping** (fixes
    `Type::nominal_class()` returning `None` on `Type::Intersection` across 9 broken sites,
    `.values().annotate()` false positives, custom `QuerySet` `.values()`, generic `QuerySet`
    wipeout in `bound_manager`, and `alias()`/`prefetch_related()` conflation).
1. **Salsa-Cached `ModelOptions` (`_meta`) + Annotations-First `resolve_path`** (eliminates ~20 of
    31 `iter_mro` walks and 3 broken `<parent>_ptr` copies; fixes positional-aggregate `order_by()`
    false positives, single-hop `values()` runtime types, and `own_model_fields` Salsa hazards).
1. **Unified `AppRegistry` + Split `project.rs` (`settings.rs` + `registry.rs`) + Inverted
    Relations** (splits 897-line `project.rs` before it breaches 1,000 lines; fixes `Meta.app_label`,
    `AppConfig.label`, `%(app_label)s` poisoning `is_complete`, `django.contrib.*` string refs,
    partial `INSTALLED_APPS` reads, and O(M × F) relation scans).
1. **Minimal Core Footprint** (evicts Django completely from `attribute_write.rs` and
    `call/bind/constructor.rs`, collapses `static_literal.rs` hooks, fixes `type_alias.rs` bypass,
    trims 5 single-use `KnownClass` variants including DRF from `ty_module_resolver`, and addresses
    the `ty_python_core` `_id` narrowing soundness bug).

Do not bolt narrow fixes for the remaining gaps onto the current structure: doing so pushes
`project.rs` (897 lines) and `relations.rs` (836 lines) past 1,000 lines and deepens the existing
`Intersection` and MRO-walk duplication. Refactoring around these 5 abstractions shrinks the net
diff by ~700–900 lines, cuts the core-crate hook surface nearly in half, and solves all remaining
gaps as natural consequences of the data model.

______________________________________________________________________

## Part 1: Real-Project Evaluation (`c559bc8c` vs `2542f029` vs `mypy 2.4.1`)

### 1.1 Headline Metrics

Measured on a 1,558-file Django 5.1 / Python 3.11 project using `mypy 2.4.1` and `django-stubs` /
`django-stubs-ext` pinned at `44457d47`:

| Metric                                               | `mypy 2.4.1`             | `ty @ 2542f029`       | `ty @ c559bc8c`        |
| ---------------------------------------------------- | ------------------------ | --------------------- | ---------------------- |
| Total diagnostics                                    | 8,174 (8,017 line-level) | 5,574                 | 5,812 (+238)           |
| Matched with `mypy` (same line and category)         | —                        | 2,996m / 2,989t       | 3,170m / 3,163t (+174) |
| `mypy`-only diagnostics                              | —                        | 4,875                 | 4,699 (−176)           |
| `ty`-only diagnostics                                | —                        | 2,450                 | 2,512 (+62)            |
| Annotated `null=True` field errors still `mypy`-only | 164                      | 164                   | 0 (164 / 164 matched)  |
| `environment.root` vs `--extra-search-path`          | —                        | —                     | Byte-identical         |
| Panics / stderr                                      | —                        | 0                     | 0                      |
| Warm wall time (3 alternating runs, Apple Silicon)   | —                        | 3.46s / 3.44s / 3.28s | 3.05s / 3.34s / 3.34s  |

### 1.2 Round 3 Diff Verification (`2542f029` → `c559bc8c`)

- **+174 newly matched with `mypy` (all real errors):** 173 `invalid-assignment` on fields
    annotated bare `models.XField` (165) or `models.XField | None` (8) with `null=True`, plus 1
    `invalid-explicit-return` on a helper returning `models.CharField(..., null=True)` from
    `-> models.CharField`.
- **−5 removed `invalid-argument-type`:** 4 real `F("...") + ...` query-expression false positives
    fixed by `4b7a0f35`, plus 1 field dropped from a `**kwargs` unpacking on a `Field | None`
    annotated attribute.
- **363 pure message updates:** `Expected <T>` → `Expected <T> | Combinable` on existing
    `invalid-argument-type` diagnostics.
- **+69 net-new `ty`-only diagnostics (0 regressions — 100% cascade from the broken annotations):**
    - 59 `invalid-argument-type` passing `None` or `T | None` to `create()`/`update()` on fields
        annotated bare `models.XField = models.XField(null=True)` (0 unannotated fields affected).
    - 9 `unknown-argument` + 1 `Cannot resolve keyword` on fields annotated `models.XField | None`,
        where rejecting the invalid assignment leaves a union type that is no longer a `Field`
        instance.

### 1.3 Confirmed Remaining Django Issues on the Real Project

Every confirmed Django issue observed on the real project reduces to 3 general gaps (plus 3 tiny
edge cases):

1. **`.values(...).annotate(...)` drops annotated keys (24 false positives, `invalid-typed-dict-key`):**

    ```py
    for row in Book.objects.values("author_id").annotate(total=Count("id")):
        count = row["total"]  # false invalid-typed-dict-key
    ```

1. **`JSONField(null=True)` drops `| None` (122 false negatives across `index`, `arg-type`,
    `union-attr`, `operator`, `dict-item`, `assignment`):**

    ```py
    class Article(models.Model):
        meta = models.JSONField(null=True)

    def f(article: Article) -> None:
        article.meta["key"]  # ty infers Any instead of Any | None
    ```

1. **Third-party `Field` subclass with `null=True` loses nullability (3 false positives):**

    ```py
    class RegionField(models.CharField):
        def __init__(self, *args, **kwargs): ...

    class Member(models.Model):
        region = RegionField(null=True)

    Member(region=None)  # false invalid-argument-type
    ```

1. **Reverse relation missed on a submodule model declaring `Meta.app_label` (2 false positives):**

    ```py
    # catalog/sub/models.py
    class Profile(models.Model):
        class Meta:
            app_label = "catalog"

    class Asset(models.Model):
        profile = models.ForeignKey("catalog.Profile", related_name="assets", on_delete=models.CASCADE)

    def f(profile: Profile) -> None:
        profile.assets  # false missing-attribute
    ```

1. **Small Django misses (5 total):** `values_list(...)` losing row typing on custom `QuerySet`
    subclasses (2), `[("", "---")] + Status.choices` list invariance with `_StrPromise` (2), and
    `helper(models.CharField(null=True))` invariance against non-nullable `CharField` (1).

### 1.4 Two Real-Project Disagreements Explicitly Ruled NOT `ty` Bugs

- **`StateApps.get_model()` in migration tests (7 hits):** `ty` already restricts `get_model()`
    refinement to `KnownClass::DjangoApps` itself (`django.rs:188-190, 212-218`), skipping
    `StateApps`. The migration helper in the project was annotated `-> Apps` instead of
    `-> StateApps`.
- **Auto-`id` on `abstract = True` models (20 hits):** Intentional, tested PR #2 feature
    (`model.rs:395-441`, `django.md:1600`). `ty` inspects concrete subclasses in the project and
    synthesizes `.id` only when every concrete subclass has an automatic `id` primary key.

### 1.5 Out-of-Scope General `ty` Issues Observed (Do Not Put in PR #2)

Recorded for separate upstream issues/PRs:

- `Protocol` callable attribute `__call__: _F` binds `self` (6 false positives, e.g.
    `pytest.skip("reason")` failing with `too-many-positional-arguments`).
- `dict.get(key)` does not report incompatible key types (15 misses).
- No duplicate-definition (`no-redef`) diagnostic in the same scope (9 misses).
- Pydantic `BaseModel.__init__` list invariance / union field checks (3 misses).
- Untyped third-party task/signature kwargs resolving as `Unknown` (3 misses).
- Generic method overridden with a concrete specialization (2 misses).

______________________________________________________________________

## Part 2: Thermo-Nuclear Architectural Audit — The 5 Holistic Abstractions

### Finding 1 (P0): Fragmented 5-Stage Field-Type Pipeline & 3 Core Constructor Hooks

**Affected files (`c559bc8c`):**

- `crates/ty_python_semantic/src/types/call/bind/constructor.rs:8, 397-405, 536-547` (file now 949
    lines)
- `crates/ty_python_semantic/src/types/call/bind.rs:1469-1495, 3530-3539`
- `crates/ty_python_semantic/src/types/dedicated/django/fields.rs:48-112, 360-370, 404-632`
- `crates/ty_python_semantic/src/types/dedicated/django/relations.rs:431-484, 675-791`
- `crates/ty_python_semantic/src/types/dedicated/django/model.rs:151-202, 384-391, 566-606`

#### Why several separate problems are the same underlying defect

Every Django field in `django-stubs` inherits `Field[_ST_contra, _GT_co]` (except
`ManyToManyField[_To, _Through]`). Across 89 commits, computing a field's `(write, read)` type
fractured into **5 disjoint stages** that disagree with one another:

1. Constructor slot-filling in `constructor.rs:536-547` (`foreign_key_model_type` in
    `relations.rs:431-484`): handles generic `ForeignKey` only, parses `null=True` on its own,
    breaks on `**kwargs` `ForeignKey` subclasses, and hacks `Any | None` for string `to`
    (`relations.rs:444`).
1. Premature constructor mutation in `call/bind.rs:3530-3539` (`constructed_instance_type` for
    `Aggregate` `default_alias`): mutates `constructor_context.instance_type` inside
    `evaluate_known_cases` before downstream constructors finish.
1. Constructor post-check override in `call/bind.rs:1469-1495` (`nullable_field_type` +
    `is_nullable_field_class` in `fields.rs:473-553`): hardcodes a single-module allowlist
    (`KnownModule::DjangoDbModelsFields` or `django.* && is_foreign_key`) and requires 2 own type
    parameters (`fields.rs:483-486`).
1. Class-member refinement in `fields.rs:404-412` (`model_field_type` → `converted_field_type` →
    `refined_field_type`).
1. Ad-hoc consumer re-assembly in `fields.rs:360` (`renamed_field`), `model.rs:151`
    (`field_keyword_parameters`), `model.rs:384` (`primary_key_member`), and `model.rs:566`
    (`foreign_key_id_type`).

#### Concrete bugs caused by this fragmentation

- **Gap 1 (`JSONField` 122 misses + third-party `*args, **kwargs` `Field` subclasses 3 FPs + `contrib.postgres`/`gis` + project
    `Field` subclasses):** `is_nullable_field_class` (`fields.rs:548-553`) excludes
    `django.db.models.fields.json.JSONField`, `django.contrib.postgres.fields.*`,
    `django.contrib.gis.db.models.fields.*`, and all third-party/user `Field` subclasses. In
    `django-stubs`, there are only **two** real Django exclusions to `null=True` widening
    `[write, read]` with `| None`: `ManyToManyField` and `FileField`/`ImageField` (whose descriptor
    wraps SQL `NULL` in a `FieldFile` proxy).
- **Converted fields drop `null=True` (`fields.rs:411, 572-632`):** `converted_field_type` never
    receives `Field` or `field.metadata.null`, so `ListField(max_length=100, null=True)` reads
    `list[str]` instead of `list[str] | None`.
- **Symbol-vs-name mismatch breaks renamed FKs in `__init__`/`update()` (`model.rs:153` vs
    `relations.rs:708-710`):** `field_keyword_parameters` passes `&field.symbol` to
    `model_field_type`, which searches `field.name == name`, missing renamed FKs completely.
- **Renamed fields and explicit PKs bypass `model_field_type` (`fields.rs:360-370`,
    `model.rs:384-391`):** Both return raw `field.ty`, dropping string-FK refinement and
    `from_db_value` conversion.
- **Abstract `"self"` FK and abstract M2M `through` break (`relations.rs:711, 791`):**
    `refined_field_type` passes `(class, class)` instead of `(declaring, model)`, resolving `"self"`
    on an abstract base to the abstract base instead of the concrete subclass.
- **Shallow downstream overload walk (`constructor.rs:397-405`):**
    `matching_overloads_with_downstream` descends only 1 level, dropping `__init__` on metaclass
    `__call__` → `__new__` → `__init__`.
- **Blind 2-param matching (`fields.rs:420-443`):** `specialized_field_type` picks the first MRO
    base with `generic_context.len() == 2` without checking those parameters map to `DjangoField`.
- **Unconditional arg-0 parsing (`fields.rs:80-83`):** `Field::from_definition` evaluates
    `call.arguments.find_argument_value("to", 0)` (`verbose_name`) on every `CharField("Title")`.

#### Holistic Abstraction

Compute `FieldKind` once, introduce one structural `rebind_django_base` primitive shared across
`Field`, `ManyToManyField`, `QuerySet`, and `BaseManager`, collapse all 3 constructor hooks into
**one** post-constructor hook in `call/bind.rs` (evicting Django from `constructor.rs` completely),
and route all 5 model-member consumers through one `Field::resolved_type`:

```rust
/// Structural classification of a Django field, computed once per `own_model_fields` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) enum FieldKind {
    /// Any `Field` subclass other than relations. `nullable_on_null` is `false` ONLY for
    /// `FileField` / `ImageField` (whose descriptor returns a `FieldFile` proxy even on SQL NULL).
    Value { nullable_on_null: bool },
    ForeignKey { is_one_to_one: bool },
    ManyToMany,
}

/// Rebind the type arguments of `known` (e.g. `DjangoField[_ST, _GT]`, `DjangoQuerySet[_M, _R]`)
/// on `class` — preserving `class` itself if its own type parameters map directly into `known`'s
/// slots, otherwise specializing the nearest MRO base whose type parameters do.
pub(super) fn rebind_django_base<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    known: KnownClass,
    new_args: &[Option<Type<'db>>],
) -> Option<Type<'db>> {
    // 1. Walk `class`'s MRO starting at `class`.
    // 2. For each base `B`, inspect `B`'s unspecialized MRO projection onto `known`.
    // 3. If `B`'s own `GenericContext` type vars map into `known`'s slots, substitute `new_args`
    //    into those slots (keeping existing args where `None`) and return `Type::instance(B[...])`.
    todo!()
}

/// Sole Django constructor return-type hook in `call/bind.rs` (evicts Django from `constructor.rs`
/// and deletes `call/bind.rs:3530` `constructed_instance_type`).
pub(in crate::types) fn constructor_return_type<'db, 'a>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    constructed: Type<'db>,
    return_type: Type<'db>,
    overloads: impl IntoIterator<Item = &'a Binding<'db>>,
    arguments: &CallArguments<'_, 'db>,
) -> Option<Type<'db>> {
    let (class, _) = constructed.nominal_class(db, env)?.static_class_literal(db)?;
    if is_aggregate_class(db, class) {
        return aggregate_with_default_alias(db, env, class, return_type, arguments);
    }
    if !inherits_from(db, class, KnownClass::DjangoField)
        || inherits_from(db, class, KnownClass::DjangoManyToManyField)
        || inherits_from(db, class, KnownClass::DjangoFileField)
    {
        return None;
    }

    let mut ty =
        fill_foreign_key_slots(db, env, class, constructed, return_type, &overloads, arguments)
            .unwrap_or(return_type);
    if has_literal_null_true(db, &overloads, arguments) {
        // Only widens classes whose OWN type vars map to `Field[_ST, _GT]`, so
        // `region: RegionField = RegionField(null=True)` stays assignable at constructor time.
        ty = widen_own_field_nullability(db, env, class, ty).unwrap_or(ty);
    }
    (ty != return_type).then_some(ty)
}

/// Single model-member field resolver used by all 5 consumers (`adjust_class_member`,
/// `renamed_field`, `field_keyword_parameters`, `primary_key_member`, `foreign_key_id_type`).
impl<'db> Field<'db> {
    pub(super) fn resolved_type(
        &self,
        db: &'db dyn Db,
        env: &ProgramEnvironment<'db>,
        declaring: StaticClassLiteral<'db>,
        model: StaticClassLiteral<'db>,
    ) -> Type<'db> {
        let mut ty = match self.kind {
            FieldKind::ManyToMany => refined_many_to_many(db, env, declaring, model, self),
            FieldKind::ForeignKey { .. } => refined_foreign_key(db, env, declaring, model, self),
            FieldKind::Value { .. } => converted_field_type(db, env, self.ty),
        }
        .unwrap_or(self.ty);

        // Apply `null=True` once at the end for non-generic subclasses (`RegionField`, `ListField`)
        // whose constructors cannot carry `[write | None, read | None]`.
        if self.metadata.null && self.kind.allows_null_value() {
            ty = ensure_nullable_field(db, env, ty);
        }
        ty
    }
}
```

______________________________________________________________________

### Finding 2 (P0): Missing `QuerySetType` / `RowShape` Codec & Pervasive `Type::Intersection` Blindness

**Affected files (`c559bc8c`):**

- `crates/ty_python_semantic/src/types.rs:2926-2954` (`Type::nominal_class` returns `None` for
    `Type::Intersection`)
- `crates/ty_python_semantic/src/types/dedicated/django/queryset.rs:115-345, 397-574`
- `crates/ty_python_semantic/src/types/dedicated/django/lookups.rs:320-433`
- `crates/ty_python_semantic/src/types/dedicated/django/managers.rs:116-192, 351, 400-424`
- `crates/ty_python_semantic/src/types/dedicated/django/model.rs:327, 532`
- `crates/ty_python_semantic/src/types/dedicated/django.rs:224-238, 281`

#### Why several separate problems are the same underlying defect

The Django layer builds `Type::Intersection` in 3 places (`bound_manager`, `annotated_queryset`,
`annotated_model`), yet `Type::nominal_class()` returns `None` for every `Type::Intersection`.
Only `lookups.rs:407-428` remembered to unwrap intersections. Meanwhile, `queryset.rs` conflates
`QuerySet`'s two type parameters (`_Model` vs `_Row`) behind `if row == model` (`queryset.rs:189`)
and `second == model` (`lookups.rs:400`), and 6 files hand-roll 8 separate MRO walks to extract
base specializations.

#### Concrete bugs shipping right now at `c559bc8c`

- **9 silent failures on annotated models, custom `QuerySet`s, and custom `Manager`s:**
    1. `django.rs:229` (`is_queryset_or_manager`) returns `false` for `CustomQS & QuerySet[...]` and
        `BookManager & Manager[...]` — breaking chained `.annotate().annotate()` / `.annotate().values()`
        on custom `QuerySet`s and all queryset refinements on unparameterized custom `Manager`s.
    1. `django.rs:281` (`report_queryset_call`) skips `save(update_fields=["bogus"])` on any
        annotated / `WithAnnotations` model.
    1. `model.rs:327` (`is_automatic_primary_key_member`) rejects `book.pk = None` on an annotated
        `book`.
    1. `model.rs:532` (`is_nullable_foreign_key_id`) disables `if book.shelf_id:` narrowing on an
        annotated `book`.
    1. `managers.rs:421` (`model_keywords_method`) stops validating keywords on
        `Book.objects.annotate(n=Count("id")).update(bogus=1)`.
    1. `managers.rs:351` (`related_manager_member`) fails on intersected models.
    1. `queryset.rs:318` (`prefetched_relation`) degrades `to_attr` to `Any` on
        `Book.objects.annotate(...).prefetch_related(Prefetch("author", to_attr="writer"))` or two
        chained `prefetch_related(Prefetch(..., to_attr=...))` calls.
    1. `queryset.rs:466` (`queryset_rows_return_type`) drops typing on
        `Book.objects.annotate(n=Count("id")).values("author__name")`.
    1. `queryset.rs:494` (`queryset_rows_return_type`) drops the `Row` named tuple on
        `Book.objects.annotate(n=Count("id")).values_list("title", named=True)`.
- **Gap 2a (`queryset.rs:189`):** `let row = if row == model { annotated } else { row };` freezes
    `_Row` when `.annotate()` follows `.values()` or `.values_list()`, causing 24
    `invalid-typed-dict-key` false positives on standard Django `GROUP BY` queries.
- **Gap 2b (`queryset.rs:457`):** `declared.known_specialization(..., DjangoQuerySet)` requires
    exact `QuerySet` equality, silently dropping `.values()`/`.values_list()` typing on every custom
    `QuerySet` subclass.
- **`bound_manager` wipes out generic custom `QuerySet` classes (`managers.rs:179-185`):** For
    `class ActiveQuerySet(models.QuerySet[M]): ...` with `objects = ActiveQuerySet.as_manager()`,
    `specialize_partial([Some(Book), None])` replaces `ActiveQuerySet[Model]` with plain
    `QuerySet[Book, Book]`, losing every custom queryset method.
- **Conflated `_Model` vs `_Row` semantics:** `alias()` writes onto `_Row` (so
    `Article.objects.alias(n=Length("title")).get().n` falsely type-checks), `prefetch_related`
    writes `to_attr` onto `_Model` (so `.order_by("book_list")` falsely type-checks), and
    `lookups.rs:400` (`second == model`) disables `Book.objects.values("title").order_by("typo")`.

#### Holistic Abstraction

Introduce one intersection-aware `unwrap_model_instance` + `base_specialization` pair in
`type_utils.rs`, and one `QuerySetType` codec cleanly separating `_Model` (SQL query namespace)
from `_Row` (Python iteration element):

```rust
/// Unwraps `NominalInstance`, `Intersection` positive elements, and `TypeVar` upper bounds.
/// Replaces 9 broken `ty.nominal_class()` / `let Type::NominalInstance` checks across `django/`.
pub(super) fn unwrap_model_instance<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    ty: Type<'db>,
) -> Option<(StaticClassLiteral<'db>, Option<Specialization<'db>>)> {
    todo!()
}

/// Intersection-aware MRO projection onto `known` (`DjangoQuerySet`, `DjangoBaseManager`, `DjangoField`).
/// Replaces 8 copy-pasted `iter_mro` loops.
pub(super) fn base_specialization<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    ty: Type<'db>,
    known: KnownClass,
) -> Option<Specialization<'db>> {
    todo!()
}

/// Explicit algebraic view of `QuerySet[_Model, _Row]` / `BaseManager[_Model]`.
pub(super) struct QuerySetType<'db> {
    pub(super) model_class: StaticClassLiteral<'db>,
    /// `_Model`: the SQL query namespace (`Model & <SQL annotations + aliases>`).
    pub(super) model_ty: Type<'db>,
    /// SQL-visible annotations and aliases decoded once from `model_ty`'s protocol intersection.
    pub(super) query_annotations: Box<[(Name, Type<'db>)]>,
    /// `_Row`: what Python iteration (`list(qs)`, `qs.first()`, `qs.get()`) yields.
    pub(super) row: RowShape<'db>,
    /// Plain `QuerySet`, 2-param generic subclass, fixed-row subclass, or `BaseManager`.
    pub(super) shell: QuerySetShell<'db>,
    pub(super) is_pure_django: bool,
}

pub(super) enum RowShape<'db> {
    /// Unprojected queryset (`_Row` peels to `model_class`, even after `alias()` or `prefetch_related()`).
    Model(Type<'db>),
    /// `values("a", "b")`: closed TypedDict with known keys.
    ValuesDict(TypedDictType<'db>),
    /// `values_list("a", "b")`: heterogeneous tuple.
    ValuesTuple(Box<[Type<'db>]>),
    /// `values_list("a", "b", named=True)`: synthesized `Row` NamedTuple.
    ValuesNamed(DynamicNamedTupleLiteral<'db>),
    /// `values_list("a", flat=True)` or dynamic `dict[str, Any]` / `Any`.
    Opaque(Type<'db>),
}

impl<'db> QuerySetType<'db> {
    /// `annotate()` updates BOTH `_Model` and `_Row`; `alias()` updates ONLY `_Model`.
    pub(super) fn with_annotations(
        mut self,
        db: &'db dyn Db,
        env: &ProgramEnvironment<'db>,
        attrs: &[(Name, Type<'db>)],
        add_to_row: bool,
    ) -> Self {
        self.model_ty = intersect_protocol(db, env, self.model_ty, attrs);
        if add_to_row {
            self.row = match self.row {
                RowShape::Model(row_ty) => {
                    RowShape::Model(intersect_protocol(db, env, row_ty, attrs))
                }
                RowShape::ValuesDict(td) => {
                    RowShape::ValuesDict(extend_typed_dict(db, td, attrs))
                }
                RowShape::ValuesTuple(cols) => {
                    RowShape::ValuesTuple(append_tuple_cols(cols, attrs))
                }
                RowShape::ValuesNamed(nt) => {
                    RowShape::ValuesNamed(extend_named_tuple(db, nt, attrs))
                }
                RowShape::Opaque(ty) => RowShape::Opaque(ty),
            };
        }
        self
    }

    /// `prefetch_related(Prefetch(..., to_attr=...))` updates ONLY `_Row`, NEVER `_Model`.
    pub(super) fn with_prefetched_attrs(
        mut self,
        db: &'db dyn Db,
        env: &ProgramEnvironment<'db>,
        attrs: &[(Name, Type<'db>)],
    ) -> Self {
        if let RowShape::Model(row_ty) = self.row {
            self.row = RowShape::Model(intersect_protocol(db, env, row_ty, attrs));
        }
        self
    }

    /// Specializes 2-param custom subclasses directly, replaces any existing `QuerySet[...]`
    /// element in an intersection instead of nesting intersections, and never creates an unsound
    /// `BookQuerySet & QuerySet[Book, TypedDict]` intersection.
    pub(super) fn into_type(self, db: &'db dyn Db, env: &ProgramEnvironment<'db>) -> Type<'db> {
        todo!()
    }
}
```

In `bound_manager`, extract the manager's existing 2nd type argument (`_QS_co`); if it is a custom
`QuerySet` subclass generic in `_Model`, rebind its `_Model` slot via `rebind_django_base` instead
of discarding it with `None`.

______________________________________________________________________

### Finding 3 (P1): Missing Salsa-Cached `ModelOptions` (`_meta`) & Split-Brain `resolve_path`

**Affected files (`c559bc8c`):**

- `crates/ty_python_semantic/src/types/dedicated/django/fields.rs:171-350`
- `crates/ty_python_semantic/src/types/dedicated/django/model.rs:113-228, 357-456, 566-668, 785-801`
- `crates/ty_python_semantic/src/types/dedicated/django/relations.rs:128-138, 195-238`
- `crates/ty_python_semantic/src/types/dedicated/django/lookups.rs:118-316, 456-579, 664-718`
- `crates/ty_python_semantic/src/types/dedicated/django/queryset.rs:79, 283-329, 461-477`
- `crates/ty_python_semantic/src/types/dedicated/django.rs:128-176`

#### Why several separate problems are the same underlying defect

There are **40 `iter_mro` walks** across `dedicated/django/` (31 in `{fields,model,relations,managers}.rs`),
each independently re-deriving pieces of Django's `Model._meta`. Meanwhile, `queryset.rs` and
`lookups.rs` split path resolution down the middle (`if name.contains("__")`).

#### Concrete bugs and Salsa hazards shipping right now at `c559bc8c`

- **4 copy-pasted `<parent>_ptr` implementations — 3 of 4 are buggy:**
    - `model.rs:205-209` and `lookups.rs:201-225` emit `<parent>_ptr` on proxy models, emit
        `<proxy>_ptr` for children of proxy models, and ignore explicit `parent_link=True` fields.
    - `model.rs:623-668` and `relations.rs:195-214` miss multi-table inheritance through an abstract
        intermediate model (`class AbstractMid(Parent): class Meta: abstract = True; class Child(AbstractMid): pass`),
        and `relations.rs:195-214` emits a duplicate reverse accessor alongside `parent_link=True`.
- **Proven false positive on positional aggregates (`lookups.rs:463-483` + `queryset.rs:79`):**
    `Review.objects.annotate(Count("article")).order_by("article__count")` splits `"article__count"`
    on `__` *before* checking `query.annotations`, resolving `"article"` as `ForeignKey(Article)`
    and then reporting a false `Cannot resolve keyword 'count' into field`. And
    `.values("article__count")` hits `name.contains("__")` at `queryset.rs:465` and drops typing.
- **Wrong runtime types for single-hop relations in `values()`/`values_list()` (`queryset.rs:465-474`):**
    - `Book.objects.values("author")` calls `model.member("author")` → types `"author"` as `Author`
        instead of `int` (`author_id`).
    - `Author.objects.values("books")` calls `model.member("books")` → types `"books"` as
        `RelatedManager[Book]` instead of `int | None`.
    - Multi-hop relation endpoints (`Chapter.objects.values("book__author")`) drop typing at
        `lookups.rs:298`.
    - `Book.objects.annotate(n=Count("id")).select_related("n")` silently skips checking at
        `lookups.rs:559-560`.
- **`own_model_fields` Salsa hazard & unnecessary work (`fields.rs:171-208`):** Has no
    `cycle_initial` and calls `binding_type(db, definition)` on every method and nested class in
    every model body before filtering to assignments — risking a Salsa cycle panic if a class-body
    value references a synthesized member of the same model. Conditional field definitions
    (`if FLAG: id = BigAutoField(...) else: id = AutoField(...)`) also produce duplicate `Field`
    entries, duplicating `IncomingRelation`s and `"Choices are: author, author"`.
- **Factory-function calls misparsed as `Field.__init__` (`fields.rs:76-111`):**
    `title = make_field("arg0", name="foo")` is parsed as `Field.__init__` without checking that the
    callee is a `DjangoField` class.
- **Stub-declared nullable FKs lose `| None` on `<field>_id` (`model.rs:601`):**
    `foreign_key_id_type` strips `None` from `__get__` and re-adds it only when `field.metadata.null`
    (an AST keyword), dropping `| None` for `.pyi` stub fields like
    `author: ForeignKey[Author | None, Author | None]`.
- **5 MRO walks per model member lookup (`django.rs:174-176`):** `contributed_attribute_type`
    (`model.rs:785-801`) runs `ty.member("contribute_to_class")` plus a full MRO walk on every Django
    `Field` and `Manager` (all of which define `contribute_to_class`), only to return `None`.
- **Backwards `report_update_fields` (`lookups.rs:664-718`):** Calls multi-hop `resolve_path` for a
    method that never accepts `__`, then spends 16 lines undoing multi-step paths.

#### Holistic Abstraction

```rust
/// Salsa-cached equivalent of Django's `Model._meta` (`Options`).
/// Replaces ~20 ad-hoc MRO walks and 4 copy-pasted `<parent>_ptr` implementations.
#[salsa::tracked(
    returns(ref),
    cycle_initial = |_, _, _| ModelOptions::default(),
    heap_size = ruff_memory_usage::heap_size,
)]
pub(super) fn model_options<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> ModelOptions<'db> {
    todo!()
}

#[derive(Debug, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) struct ModelOptions<'db> {
    pub(super) meta: ModelMeta<'db>, // is_abstract, is_proxy, app_label, default_manager_name
    pub(super) concrete_model: Option<StaticClassLiteral<'db>>,
    /// Direct concrete parents (traversing abstract bases, resolving proxies, honoring `parent_link=True`).
    pub(super) parent_links: Box<[ParentLink<'db>]>,
    /// MRO-ordered, deduplicated by `field.name`, with declaring model and `FieldKind`.
    pub(super) fields: Box<[ModelField<'db>]>,
    /// Class-body symbols removed because `field.symbol != field.name`.
    pub(super) renamed_symbols: FxHashSet<Name>,
    pub(super) primary_key: PrimaryKeySpec, // Explicit(idx) | ParentLink(idx) | Automatic
    pub(super) is_complete: bool,
}

/// Single path resolver for BOTH `lookups.rs` (`order_by`, `only`, `defer`, `select_related`)
/// AND ALL `values()` / `values_list()` columns (single-hop and multi-hop).
pub(super) fn resolve_path<'db>(
    db: &'db dyn Db,
    query: &QueryContext<'db>, // carries `(Name, Type)` annotations from `QuerySetType`
    path: &str,
) -> ResolvedPath<'db> {
    // 1. Check annotations FIRST (exact match OR `"{ann}__"` prefix) BEFORE splitting on `__`.
    //    Fixes `annotate(Count("article")).order_by("article__count")` and `.values("article__count")`.
    if let Some((ann_name, ann_ty)) = query.match_annotation(path) {
        return ResolvedPath::Annotation {
            name: ann_name,
            ty: ann_ty,
            has_transform: path.len() > ann_name.len(),
        };
    }
    // 2. Otherwise split on `__` and walk fields / parent_links / incoming_relations via `model_options`.
    todo!()
}
```

Additional cleanup in this wave:

- Filter `own_model_fields` to `Assignment` / `AnnotatedAssignment` definitions before calling
    `binding_type`, merge conditional definitions per symbol, verify the call target inherits
    `DjangoField`, and add `cycle_initial`.
- Replace the 5-walk cascade in `refined_class_member` with a single-pass `classify_model_member`.
- Replace `report_update_fields`'s `resolve_path` call with a direct `model_names(db, model).get(name)`
    check (unconditionally rejecting `__`).

______________________________________________________________________

### Finding 4 (P0): 897-Line `project.rs`, 3 Divergent App-Label Guessers, & O(M × F) Relation Scan

**Affected files (`c559bc8c`):**

- `crates/ty_python_semantic/src/types/dedicated/django/project.rs:1-897` (897 lines — 103 lines
    from 1,000)
- `crates/ty_python_semantic/src/types/dedicated/django/relations.rs:169-426, 527-624`

#### Why several separate problems are the same underlying defect

`project.rs` mixes three unrelated subsystems: settings discovery & AST evaluation (436 lines),
model/app scanning & stub mapping (364 lines), and `_ChoicesPrimitive` (53 lines, a temporary
workaround for `ty#4551`). Meanwhile, app-label and `"<app>.<Model>"` lookup is guessed
independently in 3 incompatible places (`relations.rs:560` `app_model_symbol`, `relations.rs:599`
`registered_model`, and `project.rs:595` `installed_app_models_modules`), none of which read
`Meta.app_label` or `AppConfig.label`.

#### Concrete bugs and performance hazards shipping right now at `c559bc8c`

- **Gap 3 (`Meta.app_label`, `AppConfig.label`, `%(app_label)s`):** Submodule models with
    `Meta.app_label` miss reverse relations; apps with custom `AppConfig.label` fail string lookups;
    unqualified `"Author"` fails across split `models/author.py` & `models/book.py` files.
- **`%(app_label)s` blast-radius bug (`relations.rs:278-289` → `lookups.rs:228`):** Because `ty`
    lacks `model_app_label`, any single abstract mixin using `related_name="%(app_label)s_%(class)s_set"`
    sets `is_complete = false`, **silently disabling all `order_by`/`only`/`defer`/`select_related`/
    `values`/`save(update_fields=...)` checks on the target model across the whole project**.
- **Broken `django.contrib.*` string references (`relations.rs:560-586`):**
    `ForeignKey("auth.Group")` and `ForeignKey("contenttypes.ContentType")` fail because
    `app_model_symbol` never consults installed apps, and `registered_model` misses installed apps
    too (forcing a hardcoded `"auth.User"` branch at `project.rs:791` while `apps.get_model("auth", "Group")`
    returns `type[Any]`).
- **`AppConfig` project-path bug (`project.rs:639`):** Checks `is_project_search_path` on the
    `AppConfig`'s defining module rather than its resolved `package`, silently dropping a project
    `AppConfig` subclass configuring a third-party app.
- **`INSTALLED_APPS` over-gating & missing Python idioms (`project.rs:670, 728-785`):** Line 670
    returns `None` whenever `settings.is_complete == false` (e.g. `from .local import *`),
    contradicting lines 664–667. `binding_strings` handles `StarImport` but not `ImportFrom`
    (`from .base import DJANGO_APPS`), and `expression_strings` rejects `[*DJANGO_APPS, "local"]`.
- **O(M × F) un-inverted `incoming_relations` (`relations.rs:169-347`):** For every target model,
    scans every model in the project + installed apps and resolves every relation field, discarding
    ~99% — and invalidating every model's `incoming_relations` whenever any single model is edited.
    Also `project_models` (`project.rs:294`) and `relation_source_models` (`319`) call `file_models`
    repeatedly inside filter loops, and `installed_models` (`374`) uses O(N²) dedup.
- **`symmetrical` M2M bug (`relations.rs:297-299`):** Checks `field.to.as_str() == Some("self")`,
    missing `class Person(models.Model): friends = ManyToManyField("Person")`. In Django,
    `symmetrical` defaults to `True` whenever `related == source`.
- **56 lines of verbatim duplication (`relations.rs:371-426`):** `related_manager` and
    `many_related_manager` differ only in the descriptor name and `[Some(model), None]` vs
    `[Some(to), Some(through)]`.

#### Holistic Abstraction

Split `project.rs` into **`settings.rs`** (~320 LOC) and **`registry.rs`** (~320 LOC), move
`_ChoicesPrimitive` to `fields.rs`, and invert `incoming_relations` via per-source
`outgoing_relations`:

```rust
/// Single Salsa-cached registry of Django apps (both project AND installed third-party apps).
#[salsa::tracked(returns(ref), heap_size = ruff_memory_usage::heap_size)]
pub(super) fn app_registry<'db>(db: &'db dyn Db, program: Program<'db>) -> AppRegistry<'db> {
    todo!()
}

pub(super) struct InstalledApp<'db> {
    pub(super) label: Name,                        // `AppConfig.label`, else `package.last_component()`
    pub(super) package: ModuleName,                // `AppConfig.name`, else entry
    pub(super) is_project: bool,                   // checked on `package`, NOT `AppConfig`'s module
    pub(super) source_models_files: Box<[File]>,   // real `.py` files (for field args & `related_name`)
    pub(super) imported_models_file: Option<File>, // stub/import file (cached once)
}

/// Single source of truth for any model's app label:
/// 1. `model_options(db, model).meta.app_label` (`class Meta: app_label = "..."`)
/// 2. Longest-prefix match of `model`'s module in `app_registry(db, program)`
/// 3. Fallback to package segment before `models`
pub(super) fn model_app_label<'db>(db: &'db dyn Db, model: StaticClassLiteral<'db>) -> Option<Name> {
    todo!()
}

/// Single model lookup replacing `app_model_symbol`, `registered_model`, and the `"auth.User"` hardcode.
pub(super) fn lookup_model<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    app_label: &str,
    model_name: &str, // case-insensitive, matching Django's `Apps.get_model`
    origin_file: Option<ProgramFile<'db>>,
) -> Option<Type<'db>> {
    todo!()
}

/// Per-source outgoing relations (cached per model; O(F) instead of O(M × F)).
#[salsa::tracked(returns(deref), heap_size = ruff_memory_usage::heap_size)]
fn outgoing_relations<'db>(
    db: &'db dyn Db,
    source: StaticClassLiteral<'db>,
) -> Box<[OutgoingRelation<'db>]> {
    todo!()
}
```

______________________________________________________________________

### Finding 5 (P0): Core-Crate Leaks, Eliminable Hooks, & `ty_python_core` Soundness Bug

**Affected files (`c559bc8c`):**

- `crates/ty_python_semantic/src/types/attribute_write.rs:18, 917-927`
- `crates/ty_python_semantic/src/types/call/bind/constructor.rs:8, 536-547`
- `crates/ty_python_semantic/src/types/class/static_literal.rs:1719-1740`
- `crates/ty_python_semantic/src/types/type_alias.rs:372-376` vs `603-663`
- `crates/ty_python_semantic/src/types/class/known.rs:170-178`, `slots.rs:184-192`,
    `crates/ty_module_resolver/src/module.rs:280-311, 371-388, 423-438`
- `crates/ty_python_core/src/place.rs:37-51, 568-593, 685-691`, `builder.rs:1137-1143, 1305-1322, 2372-2395`,
    `member.rs:312-318`
- `crates/ty_python_semantic/src/types/dedicated/django.rs:128-151`

#### Concrete simplifications and core-boundary bug fixes

1. **Delete Django from `attribute_write.rs` completely:** `AutoField` in `django-stubs` is already
    a 2-param `Field[_ST, _GT]` descriptor (`AutoField[int | str, int]`). Specializing synthesized
    automatic PKs to `AutoField[write | None, read]` in `model.rs:451` makes `book.id = None` and
    `book.pk = None` work natively through `AutoField.__set__` while keeping reads as `int`. This
    deletes `attribute_write.rs:917-927`, `enum PrimaryKeyMember`, and
    `is_automatic_primary_key_member`.
1. **Collapse `static_literal.rs:1728 & 1737` into one `django::adjust_class_member` call** outside
    the unrelated `__new__ | __init__` match, checking rename-hiding *before* running refinement.
    Also delete the buggy 8-line name pre-filter in `django.rs:128-140`, which currently makes
    `renamed_field` unreachable for `name="legacy_id"`, `name="id"`, or `name="get_foo_display"`.
1. **Fix `type_alias.rs:374` bypass bug:** `TypeAliasType::value_type_with_recursion` (`647`) calls
    `specialized_value_type` directly, bypassing `ManualPEP695TypeAliasType::value_type` so
    `type[WithAnnotations[Article, TitleLength]]` loses its override, and PEP 695 syntax is missed.
    Move the hook into `TypeAliasType::specialized_value_type` (`603`) and call
    `annotations.resolve_type_alias(db)` in `queryset.rs:378`.
1. **Trim 5 single-use cold-path `KnownClass` variants** (`DjangoBaseForm`, `DrfBaseSerializer`,
    `DjangoApps`, redundant `DjangoAggregate`, `DjangoOneToOneField`) from 2,542-line `known.rs`,
    `slots.rs`, and `module.rs`. Replace them with a local `is_class(db, class, module, name)`
    helper inside `dedicated/django/`, removing DRF from `ty_module_resolver::KnownModule`.
1. **Address `ty_python_core` `_id` companion place soundness bug & global pollution:**
    - **Soundness bug:** `record_binding_with` (`builder.rs:1727`) does not invalidate narrowed
        `book.shelf` when `book.shelf_id = None` is assigned, leaving `book.shelf` narrowed to `Shelf`
        after clearing the FK.
    - **Over-broad matching:** `place.rs:686` registers companion places even for
        `a.x_id is b.y_id` (no `None` operand), mutating `PlaceTable` across all non-Django Python
        codebases via an awkward `(PossiblyNarrowedPlaces, SmallVec)` tuple side-channel.
    - **Decision:** Either fix reassignment invalidation + require a `None` operand in
        `expr_compare` + pass `&mut PlaceTableBuilder` directly, or keep isolated to drop if upstream
        maintainers object to touching `ty_python_core`.

```rust
// 1. DELETE `attribute_write.rs:917-927` via `AutoField[write | None, read]`:
let auto_pk = rebind_django_base(
    db,
    env,
    auto_field_class,
    KnownClass::DjangoField,
    &[
        Some(UnionType::from_two_elements(db, write, Type::none(db))),
        Some(read),
    ],
);

// 2. COLLAPSE `static_literal.rs:1728 & 1737` into one call outside `__new__ | __init__`:
let member = self.class_member_with_symbol(db, env, name, symbol, member_lookup_policy);
let member = django::adjust_class_member(db, env, self, specialization, name, member);

// 3. TRIM 5 cold-path `KnownClass` variants with a local Django helper:
pub(super) fn is_class(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
    module_path: &str,
    class_name: &str,
) -> bool {
    class.name(db) == class_name
        && file_to_module(db, class.program_file(db).resolver_file(db))
            .is_some_and(|m| m.name(db).as_str() == module_path)
}
```

______________________________________________________________________

## Part 3: Core Hook Audit, Target Module Layout, & Execution Plan

### 3.1 Verdict on Every Core Hook

| Core File (`c559bc8c`)                                   | Hook / Change                                       | Verdict            | Action                                                                                |
| -------------------------------------------------------- | --------------------------------------------------- | ------------------ | ------------------------------------------------------------------------------------- |
| `types/attribute_write.rs:918`                           | `django::is_automatic_primary_key_member`           | **DELETE**         | Specialize automatic `AutoField` to `AutoField[write \| None, read]` in `model.rs`.   |
| `types/call/bind/constructor.rs:541`                     | `django::is_foreign_key` + `foreign_key_model_type` | **DELETE**         | Move into unified `django::constructor_return_type` in `call/bind.rs`.                |
| `types/call/bind.rs:3531`                                | `django::constructed_instance_type`                 | **COLLAPSE**       | Merge into `override_constructor_return_types`.                                       |
| `types/call/bind.rs:1469-1495`                           | `override_constructor_return_types`                 | **KEEP & EXPAND**  | Sole Django constructor hook (`ForeignKey`, `Field(null=True)`, `Aggregate`).         |
| `types/call/bind.rs:2058`                                | `django::method_return_type`                        | **KEEP**           | Clean single method-return-type boundary.                                             |
| `types/class/static_literal.rs:1728,1737`                | `refined_class_member` + `is_renamed_field`         | **COLLAPSE**       | Merge into `django::adjust_class_member`; check rename-hiding first.                  |
| `types/class/static_literal.rs:1887`                     | `django::synthesized_member`                        | **KEEP**           | Clean synthesis boundary; delete buggy `django.rs:128-140` pre-filter.                |
| `ty_python_core/{place,builder,member}.rs` + `narrow.rs` | `_id` companion place narrowing                     | **FIX OR ISOLATE** | Fix reassignment invalidation + require `None` + drop tuple side-channel, or isolate. |
| `types/type_alias.rs:374`                                | `django::type_alias_value_override`                 | **MOVE (FIX)**     | Move to `TypeAliasType::specialized_value_type` (`603`).                              |
| `types/infer/builder/type_expression.rs:177`             | `django::type_alias_override`                       | **KEEP (RENAME)**  | Rename to `legacy_type_alias_override`; track `_ChoicesPrimitive` under `ty#4551`.    |
| `types/infer/builder.rs:10295`                           | `django::report_queryset_call`                      | **KEEP (RENAME)**  | Rename to `report_call_diagnostics` (covers `save(update_fields=...)` too).           |
| `types/overrides/attributes.rs:473`                      | `django::has_meta_options`                          | **KEEP**           | Minimal 1-line hook; drop `DjangoBaseForm` & `DrfBaseSerializer` from `KnownClass`.   |
| `types/class/known.rs`, `slots.rs`, `module.rs`          | +9 `KnownClass`, +13 `KnownModule`                  | **TRIM**           | Demote 5 single-use cold-path classes to local checks inside `dedicated/django/`.     |
| `types/class/dynamic_literal.rs`, `named_tuple.rs`       | `Dynamic*Anchor::Implicit`                          | **KEEP**           | Clean generic infrastructure, zero Django coupling.                                   |

### 3.2 Target Module Layout (Every File Below ~550 Lines)

Currently 4 touched files crowd the 1,000-line ceiling (`constructor.rs` 949, `project.rs` 897,
`relations.rs` 836, `model.rs` 801). The 5 unifications delete ~700–900 lines of duplication, pull
every module comfortably below ~550 lines, and align 1:1 with how PR #2 splits into an upstream PR
stack:

| Module                 | Responsibility                                                                                                                   |              Est. LOC |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------- | --------------------: |
| `django.rs`            | Public hook facade (~7 exports, down from 13)                                                                                    |                  ~200 |
| `django/type_utils.rs` | `inherits_from`, `is_class`, `unwrap_model_instance`, `base_specialization`, `rebind_django_base`, `classify_model_member`       |                  ~180 |
| `django/options.rs`    | Salsa-tracked `ModelOptions` (`_meta`, `parent_links`, merged `fields`, `PrimaryKeySpec`)                                        |                  ~350 |
| `django/fields.rs`     | `Field`, `FieldKind`, `own_model_fields`, unified `Field::resolved_type`, structural `nullable_field_type`, `_ChoicesPrimitive`  |        ~350 (was 632) |
| `django/settings.rs`   | `ProjectSettings`, `manage.py` discovery, `INSTALLED_APPS`, `AUTH_USER_MODEL`, `setting()`                                       | ~320 (split from 897) |
| `django/registry.rs`   | Salsa-tracked `AppRegistry`, `AppConfig` + `Meta.app_label`, `model_app_label`, `lookup_model`, `file_model_bindings`            | ~320 (split from 897) |
| `django/relations.rs`  | Per-source `outgoing_relations` + inverted `incoming_relations`, `reverse_accessor`, single manager helper                       |        ~280 (was 836) |
| `django/model.rs`      | Thin consumers of `ModelOptions` + `Field::resolved_type` (`init_method`, `primary_key_member`, `_id`, exceptions, display/date) |        ~420 (was 801) |
| `django/managers.rs`   | `bound_manager` (preserving generic `QuerySet`s), `from_queryset`, `default_manager`, `model_keywords_method`                    |        ~340 (was 494) |
| `django/queryset.rs`   | `QuerySetType` + `RowShape` codec (`annotate`, `alias`, `prefetch_related`, `values`, `values_list`, `WithAnnotations`)          |        ~500 (was 611) |
| `django/lookups.rs`    | Annotations-first `resolve_path` + queryset/`save(update_fields=...)` diagnostics                                                |        ~550 (was 718) |

### 3.3 Recommended Execution Waves (TDD, Each Wave Ends Green)

Start with a failing-tests commit covering Gaps 1–3 and every newly discovered bug above, then
execute in 5 verifiable waves (checking `cargo fmt --all`, `CARGO_BUILD_WARNINGS=deny cargo clippy`,
and `MDTEST_EXTERNAL=1 cargo test -p ty_python_semantic -p ty_ide -p ty_python_core` after each):

1. **Wave 1 — Core boundary cleanup:** Delete `attribute_write.rs` hook via
    `AutoField[write | None, read]`, collapse `static_literal.rs` member hooks + delete
    `django.rs:128-140` pre-filter, move `type_alias.rs:374` hook to `specialized_value_type`, trim
    the 5 single-use `KnownClass` variants.
1. **Wave 2 — `type_utils.rs` + `QuerySetType` codec (`queryset.rs`, `lookups.rs`, `managers.rs`):**
    Fixes Gap 2 (`.values().annotate()` + custom `QuerySet` `.values()`), all 9 `Intersection`
    bugs, generic `QuerySet` wipeout in `bound_manager`, `alias()`/`prefetch_related()` isolation,
    positional-aggregate `order_by("article__count")` false positive, and single-hop `values()`
    relation types.
1. **Wave 3 — `options.rs` + unified Field pipeline + single constructor hook:** Evicts Django from
    `call/bind/constructor.rs`, collapses 3 constructor hooks into 1, introduces Salsa-cached
    `ModelOptions`, fixes Gap 1 (`JSONField`, third-party `*args, **kwargs` fields,
    `contrib.postgres`/`gis`, custom `Field` subclasses), fixes 3 broken `<parent>_ptr` copies,
    fixes `converted_field_type(null=True)` and renamed-FK bugs, fixes stub nullable `<field>_id`,
    and fixes `own_model_fields` cycle safety.
1. **Wave 4 — Split `project.rs` into `settings.rs` + `registry.rs` + invert `relations.rs`:**
    Fixes Gap 3 (`Meta.app_label`, `AppConfig.label`, `%(app_label)s`), fixes `django.contrib.*`
    string refs, stops `%(app_label)s` from poisoning `is_complete`, un-gates partial
    `INSTALLED_APPS` reads (`from .local import *`, `ImportFrom`, `Expr::Starred`), fixes
    `is_symmetrical`, and reduces `incoming_relations` from O(M × F) to O(F).
1. **Wave 5 (decision gate) — `ty_python_core` `_id` narrowing:** Either fix `book.shelf_id = None`
    invalidation + tighten `place.rs:686` to require `None` + remove the tuple side-channel, or keep
    isolated for upstream maintainer sign-off.

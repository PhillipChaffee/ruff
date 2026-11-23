# Django

Django support in ty currently relies on the stubs from [django-stubs]:

```toml
[environment]
python-version = "3.13"
python-platform = "linux"

[project]
dependencies = [
    "django-stubs @ git+https://github.com/typeddjango/django-stubs.git@44457d47e03845c27b560936ef0455d5dfc7b7d8",
    "djangorestframework==3.18.3",
    "djangorestframework-stubs==3.18.2",
]
```

## Basic model

Define a basic model by subclassing Django's `Model`:

```py
from datetime import date
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)
    published = models.DateField()
    pages = models.IntegerField()
```

Create an instance of this model:

```py
book = Book(
    title="The Hitchhiker's Guide to the Galaxy",
    published=date(1979, 10, 12),
    pages=180,
)

book.save()
```

Attribute access yields types based on the field definitions:

```py
reveal_type(book.title)  # revealed: str
reveal_type(book.published)  # revealed: date
reveal_type(book.pages)  # revealed: int
```

Attribute assignments are checked against the field's *input* type:

```py
book.title = "The Restaurant at the End of the Universe"
book.pages = 208

book.published = "1980-10-12"

book.pages = models.F("pages") + 1

book.published = 123  # error: [invalid-assignment]
book.pages = None  # error: [invalid-assignment]
```

Fields can be omitted during construction (and assigned before saving), so this is also fine:

```py
Book(
    title="The Hitchhiker's Guide to the Galaxy",
    pages=180,
)
```

Invalid field types lead to a type error:

```py
Book(
    title="The Hitchhiker's Guide to the Galaxy",
    published=123,  # error: [invalid-argument-type]
    pages=180,
)
```

Unrecognized keyword arguments also lead to an error:

```py
Book(
    title="The Hitchhiker's Guide to the Galaxy",
    publised=date(1979, 10, 12),  # error: [unknown-argument]
    pages=180,
)
```

## Constructor arguments

Besides its fields, a model accepts keywords for the primary key, for the ID attribute of a foreign
key, and for its properties:

```py
from django.db import models

class Author(models.Model):
    name = models.CharField(max_length=100)

class Book(models.Model):
    title = models.CharField(max_length=200)
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

    @property
    def label(self) -> str:
        return self.title

    @label.setter
    def label(self, value: str) -> None:
        self.title = value

def _(author: Author):
    Book(id=1, title="Dune", author=author)  # ok
    Book(pk=1, author_id=author.pk, label="Dune")  # ok

    Book(author="Frank Herbert")  # error: [invalid-argument-type]
    Book(describe="Dune")  # error: [unknown-argument]
```

Positional arguments are not checked:

```py
Book(1, "Dune", 1)  # ok
```

A model also accepts the fields of its base classes, and the link to a concrete parent:

```py
class Timestamped(models.Model):
    created = models.DateTimeField()

    class Meta:
        abstract = True

class Ebook(Book, Timestamped):
    size = models.IntegerField()

def _(book: Book):
    Ebook(title="Dune", created="2024-01-01", size=1, book_ptr=book)  # ok
    Ebook(size=None)  # error: [invalid-argument-type]
    Ebook(pages=1)  # error: [unknown-argument]
```

A model that defines `__init__` is checked against that method instead:

```py
from typing import Any

class Custom(models.Model):
    title = models.CharField(max_length=200)

    def __init__(self, *args: Any, source: str = "", **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)

class CustomChild(Custom):
    pass

Custom(source="import", anything=1)  # ok
CustomChild(source="import", anything=1)  # ok
```

A base class whose type we do not know can add fields and properties, so a model with such a base
keeps the declared constructor, which accepts any keyword:

```py
def mixin() -> Any: ...

class Tracked(mixin(), models.Model):
    title = models.CharField(max_length=200)

Tracked(tracked_at="2024-01-01")  # ok
```

`create()` and `acreate()` pass their keywords to the constructor, so they are checked in the same
way:

```py
def _(author: Author):
    reveal_type(Book.objects.create(title="Dune", author=author))  # revealed: Book
    Book.objects.create(titel="Dune")  # error: [unknown-argument]
    Book.objects.create(author="Frank Herbert")  # error: [invalid-argument-type]

    Custom.objects.create(source="import", anything=1)  # ok

async def _():
    await Book.objects.acreate(titel="Dune")  # error: [unknown-argument]
```

So does `create()` on a `QuerySet`:

```py
def _():
    Book.objects.filter(title="Dune").create(titel="Dune")  # error: [unknown-argument]
```

`update()` does not call the constructor, so it takes the fields of a model that defines `__init__`,
not the parameters of that method:

```py
Custom.objects.update(title="Dune")  # ok
Custom.objects.update(source="import")  # error: [unknown-argument]
```

The same holds for a model that inherits `__init__`:

```py
CustomChild.objects.update(title="Dune")  # ok
CustomChild.objects.update(source="import")  # error: [unknown-argument]
```

The model's properties are keywords, but not the attributes that every object has:

```py
Book(__class__=Book)  # error: [unknown-argument]
```

## Nullable fields

`null=True` adds `None` to the field's value type.

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200, null=True)
    pages = models.IntegerField(null=True)

def _(book: Book):
    reveal_type(book.title)  # revealed: str | None
    reveal_type(book.pages)  # revealed: int | None
```

## Annotated fields

The arguments of a field determine its value types, also when the field has an annotation.
`null=True` adds `None` to them, so a nullable field does not match an annotation without `None`:

```py
from django.db import models

class Settings(models.Model):
    # error: [invalid-assignment] "Object of type `BooleanField[bool | None, bool | None]` is not assignable to `BooleanField[bool, bool]`"
    enabled: models.BooleanField = models.BooleanField(null=True)
    # error: [invalid-assignment] "Object of type `CharField[str | int | None, str | None]` is not assignable to `CharField[str | int, str]`"
    code: models.CharField = models.CharField(max_length=5, null=True)
    # error: [invalid-assignment] "Object of type `BooleanField[bool | None, bool | None]` is not assignable to `BooleanField[bool, bool] | None`"
    visible: models.BooleanField | None = models.BooleanField(null=True)
```

The annotation still determines the type of the attribute:

```py
def _(settings: Settings):
    reveal_type(settings.enabled)  # revealed: bool
```

An annotation that includes `None`, an annotation of a non-nullable field, and a field without an
annotation are fine:

```py
from typing import Any

class Preferences(models.Model):
    enabled: models.BooleanField[bool | None, bool | None] = models.BooleanField(null=True)  # ok
    code: "models.CharField[str | int | None, str | None]" = models.CharField(max_length=5, null=True)  # ok
    archived: models.BooleanField = models.BooleanField(null=False)  # ok
    hidden: models.BooleanField = models.BooleanField()  # ok
    pinned = models.BooleanField(null=True)  # ok
    loose: models.Field = models.BooleanField(null=True)  # ok
    anything: Any = models.BooleanField(null=True)  # ok

def _(preferences: Preferences):
    reveal_type(preferences.enabled)  # revealed: bool | None
    reveal_type(preferences.pinned)  # revealed: bool | None
```

`null=True` also adds `None` to the values of the base class `Field`, whose value types are `Any`:

```py
class Attachment(models.Model):
    payload = models.Field(null=True)

def _(attachment: Attachment):
    reveal_type(attachment.payload)  # revealed: Any | None
```

`null` must be a literal for the stubs to choose a nullable type. Otherwise the annotation is used:

```py
def make_flag(nullable: bool) -> None:
    flag: models.BooleanField = models.BooleanField(null=nullable)  # error: [deprecated]
```

`null=True` also decides over explicit type arguments in the call that leave out `None`, because a
`NULL` column reads as `None` whatever the call says:

```py
class Profile(models.Model):
    # error: [invalid-assignment] "Object of type `BooleanField[bool | None, bool | None]` is not assignable to `BooleanField[bool, bool]`"
    confirmed: models.BooleanField = models.BooleanField[bool, bool](null=True)
    seen = models.BooleanField[bool, bool](null=True)

def _(profile: Profile):
    reveal_type(profile.seen)  # revealed: bool | None
```

A field subclass that declares its type arguments, and a field that converts its values, keep their
types:

```py
class StrictBooleanField(models.BooleanField[bool, bool]): ...

class ListField(models.CharField):
    def from_db_value(self, value: Any, expression: Any, connection: Any) -> list[str]:
        return value.split(",")

class Catalog(models.Model):
    strict: StrictBooleanField = StrictBooleanField(null=True)  # ok
    tags: ListField = ListField(max_length=100, null=True)  # ok
```

A nullable foreign key holds `None` as well:

```py
class Author(models.Model):
    pass

class Book(models.Model):
    editor: "models.ForeignKey[Author | None, Author | None]" = models.ForeignKey(
        Author, null=True, on_delete=models.SET_NULL
    )  # ok
    # error: [invalid-assignment] "Object of type `ForeignKey[Author | None, Author | None]` is not assignable to `ForeignKey[Author, Author]`"
    author: "models.ForeignKey[Author, Author]" = models.ForeignKey(Author, null=True, on_delete=models.SET_NULL)
```

A `FileField` reads as a `FieldFile` even for a `NULL` column, so `null=True` does not change its
types:

```py
class Upload(models.Model):
    document: models.FileField = models.FileField(null=True)  # ok
```

An inherited annotation is the type context of an unannotated field in a subclass:

```py
class Publication(models.Model):
    is_public: models.BooleanField = models.BooleanField(default=False)

    class Meta:
        abstract = True

class Pamphlet(Publication):
    # error: [invalid-assignment] "Object of type `BooleanField[bool | None, bool | None]` is not assignable to `BooleanField[bool, bool]`"
    is_public = models.BooleanField(null=True)
```

The same applies to a field outside a model:

```py
class Defaults:
    # error: [invalid-assignment] "Object of type `BooleanField[bool | None, bool | None]` is not assignable to `BooleanField[bool, bool]`"
    enabled: models.BooleanField = models.BooleanField(null=True)

def _():
    # error: [invalid-assignment] "Object of type `BooleanField[bool | None, bool | None]` is not assignable to `BooleanField[bool, bool]`"
    flag: models.BooleanField = models.BooleanField(null=True)
    other = models.BooleanField(null=True)
    reveal_type(other)  # revealed: BooleanField[bool | None, bool | None]
```

## Field names

The `name` parameter overrides the name of the attribute:

```py
from django.db import models

class Article(models.Model):
    views = models.IntegerField(name="view_count")

article = Article(view_count=1)

reveal_type(article.view_count)  # revealed: int
article.view_count = 2  # ok
article.view_count = None  # error: [invalid-assignment]

# The original field name is not available as an attribute.
article.views  # error: [missing-attribute]

# This is correct:
Article(view_count=1)

# Using the original attribute name is incorrect.
Article(views=1)  # error: [unknown-argument]
```

## Primary keys

### Automatic primary key

Django adds an integer `id` field when no primary key is declared. The `pk` attribute is an alias
for the primary key.

```py
from django.db import models

class Book(models.Model):
    title = models.CharField()

def _(book: Book):
    reveal_type(book.id)  # revealed: int
    reveal_type(book.pk)  # revealed: int
```

Django allows clearing an automatic primary key when copying a model instance.

```py
def _(book: Book, key: int | None):
    book.pk = None
    book.id = None
    reveal_type(book.pk)  # revealed: int
    reveal_type(book.id)  # revealed: int
    book.pk = key
    book.pk = [1]  # error: [invalid-assignment]
    book.id = [1]  # error: [invalid-assignment]
```

A subclass inherits the primary key behavior of its parent.

```py
class Ebook(Book):
    pass

def _(ebook: Ebook):
    reveal_type(ebook.id)  # revealed: int
```

### Custom primary key

An explicit primary key determines the type of `pk` and prevents Django from adding `id`.

```py
from django.db import models

class Book(models.Model):
    isbn = models.CharField(max_length=13, primary_key=True)

def _(book: Book):
    reveal_type(book.isbn)  # revealed: str
    reveal_type(book.pk)  # revealed: str
    book.pk = None  # error: [invalid-assignment]

    # This is an error now, `id` is not created when a custom primary key is defined.
    book.id  # error: [missing-attribute]
```

Subclassing a model with a custom primary key inherits the primary key behavior and does not
introduce an automatic `id` field.

```py
class Ebook(Book):
    pass

def _(ebook: Ebook):
    reveal_type(ebook.isbn)  # revealed: str
    ebook.id  # error: [missing-attribute]
```

### Explicit `id` field

An explicitly declared `id` keeps its field type:

```py
from django.db import models

class Book(models.Model):
    id = models.CharField(max_length=13, primary_key=True)

class Ebook(Book):
    pass

def _(book: Book, ebook: Ebook):
    reveal_type(book.id)  # revealed: str
    reveal_type(ebook.id)  # revealed: str
```

### ForeignKey and OneToOneField as primary keys

When a `OneToOneField` or `ForeignKey` is the primary key, `pk` has the value type of the related
model's primary key. In the following example, `Book.id` is a UUID field, so `Ebook.pk` and
`Review.pk` have type `UUID`, while their `book` attributes have type `Book`.

```py
from django.db import models

class Book(models.Model):
    id = models.UUIDField(primary_key=True)
    isbn = models.CharField(max_length=13, unique=True)

class Ebook(models.Model):
    book = models.OneToOneField(Book, on_delete=models.CASCADE, primary_key=True)

class Review(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE, primary_key=True)

def _(ebook: Ebook, review: Review):
    reveal_type(ebook.book)  # revealed: Book
    reveal_type(ebook.book_id)  # revealed: UUID
    reveal_type(ebook.pk)  # revealed: UUID
    ebook.id  # error: [missing-attribute]

    reveal_type(review.book)  # revealed: Book
    reveal_type(review.book_id)  # revealed: UUID
    reveal_type(review.pk)  # revealed: UUID
    review.id  # error: [missing-attribute]
```

With `to_field`, both `pk` and `<field>_id` have the referenced field's value type, even when it
differs from the related model's primary key. Here, `to_field="isbn"` makes `pk` and `book_id` have
type `str`, while `Book.pk` still has type `UUID`.

```py
class EbookByIsbn(models.Model):
    book = models.OneToOneField(Book, to_field="isbn", on_delete=models.CASCADE, primary_key=True)

class ReviewByIsbn(models.Model):
    book = models.ForeignKey(Book, to_field="isbn", on_delete=models.CASCADE, primary_key=True)

def _(book: Book, ebook: EbookByIsbn, review: ReviewByIsbn):
    reveal_type(book.pk)  # revealed: UUID

    reveal_type(ebook.book)  # revealed: Book
    reveal_type(ebook.book_id)  # revealed: str
    reveal_type(ebook.pk)  # revealed: str

    reveal_type(review.book)  # revealed: Book
    reveal_type(review.book_id)  # revealed: str
    reveal_type(review.pk)  # revealed: str
```

## Foreign key IDs

### Automatic primary key

A `ForeignKey` field `author` can be used to access the referenced `Author` instance via `.author`.
It also creates an `.author_id` attribute which contains that instance's primary-key value:

```py
from django.db import models

class Author(models.Model):
    pass

class Book(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
    editor = models.ForeignKey(to=Author, on_delete=models.SET_NULL, null=True, related_name="edited_books")

def _(book: Book):
    reveal_type(book.author)  # revealed: Author
    reveal_type(book.author_id)  # revealed: int

    reveal_type(book.editor)  # revealed: Author | None
    reveal_type(book.editor_id)  # revealed: int | None

    book.editor = None
    book.author = None  # error: [invalid-assignment]
```

A generic subclass of `ForeignKey` behaves like `ForeignKey`:

```py
class TrackedForeignKey[ST, GT](models.ForeignKey[ST, GT]):
    pass

class Chapter(models.Model):
    book = TrackedForeignKey(Book, on_delete=models.CASCADE)

def _(chapter: Chapter):
    reveal_type(chapter.book)  # revealed: Book
    reveal_type(chapter.book_id)  # revealed: int
```

So does a subclass that does not declare type parameters:

```py
class PlainForeignKey(models.ForeignKey):
    pass

class Page(models.Model):
    book = PlainForeignKey(Book, on_delete=models.CASCADE)
    previous = PlainForeignKey("self", null=True, on_delete=models.SET_NULL)

def _(page: Page):
    reveal_type(page.book)  # revealed: Book
    reveal_type(page.book_id)  # revealed: int
    reveal_type(page.previous)  # revealed: Page | None

    page.book = None  # error: [invalid-assignment]
```

A `ForeignKey` adds a reverse accessor to the related model. By default, its name is the lowercase
source model name followed by `_set`. The `related_name` parameter overrides this name:

```py
def _(author: Author):
    reveal_type(author.book_set.all())  # revealed: QuerySet[Book, Book]
    reveal_type(author.edited_books.all())  # revealed: QuerySet[Book, Book]
```

Setting `related_name="+"` disables the reverse accessor:

```py
class Review(models.Model):
    reviewer = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="+")

def _(author: Author):
    author.review_set  # error: [missing-attribute]
```

### Custom primary key

The type of `<field>_id` follows the related model's primary key, which need not be an integer:

```py
from django.db import models

class Book(models.Model):
    isbn = models.CharField(max_length=13, primary_key=True)

class Review(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE)

def _(review: Review):
    reveal_type(review.book)  # revealed: Book

    reveal_type(review.book_id)  # revealed: str
```

### Explicit target field

With `to_field`, `<field>_id` contains the value of that field instead of the primary key. In the
following example, the foreign key refers to the `isbn` field, not to `Book`'s primary key (`id`):

```py
from django.db import models

class Book(models.Model):
    id = models.IntegerField(primary_key=True)
    isbn = models.CharField(max_length=13, unique=True)

class Review(models.Model):
    book = models.ForeignKey(Book, to_field="isbn", on_delete=models.CASCADE)

def _(review: Review):
    reveal_type(review.book)  # revealed: Book

    reveal_type(review.book_id)  # revealed: str
```

### String references

The related model can be given as the name of a model in the same module, or as `"self"` for the
model that declares the field:

```py
from django.db import models

class Author(models.Model):
    mentor = models.ForeignKey("self", null=True, on_delete=models.SET_NULL)

class Book(models.Model):
    author = models.ForeignKey("Author", on_delete=models.CASCADE)
    editor = models.ForeignKey("Author", null=True, on_delete=models.SET_NULL)
    cover = models.OneToOneField("Cover", null=True, on_delete=models.SET_NULL)

class Cover(models.Model):
    isbn = models.CharField(max_length=13, primary_key=True)

def _(book: Book, author: Author):
    reveal_type(book.author)  # revealed: Author
    reveal_type(book.editor)  # revealed: Author | None
    reveal_type(author.mentor)  # revealed: Author | None
    reveal_type(book.cover)  # revealed: Cover | None

    book.author = None  # error: [invalid-assignment]
    book.editor = None  # ok
```

The type of `<field>_id` follows the primary key of the related model:

```py
def _(book: Book):
    reveal_type(book.author_id)  # revealed: int
    reveal_type(book.editor_id)  # revealed: int | None
    reveal_type(book.cover_id)  # revealed: str | None
```

We do not resolve the name of something that is not a model, or a reference to an app that we cannot
find. A foreign key declared with `null=True` is still nullable:

```py
class Review(models.Model):
    book = models.ForeignKey("unknown_app.Book", on_delete=models.CASCADE)
    editor = models.ForeignKey("unknown_app.Author", null=True, on_delete=models.SET_NULL)
    missing = models.ForeignKey("Missing", on_delete=models.CASCADE)

def _(review: Review):
    reveal_type(review.book)  # revealed: Any
    reveal_type(review.editor)  # revealed: Any | None
    reveal_type(review.missing)  # revealed: Any
```

## Narrowing a foreign key by its ID

Code often checks the ID of a nullable foreign key, because that needs no database query. When the
ID is set, the related object exists:

```py
from django.db import models

class Shelf(models.Model):
    label = models.CharField(max_length=20)

class Book(models.Model):
    shelf = models.ForeignKey(Shelf, null=True, on_delete=models.SET_NULL)

def _(book: Book):
    reveal_type(book.shelf)  # revealed: Shelf | None

    if book.shelf_id:  # error: [truthiness-test-of-none-union]
        reveal_type(book.shelf)  # revealed: Shelf

    if book.shelf_id is not None:
        reveal_type(book.shelf)  # revealed: Shelf
    else:
        reveal_type(book.shelf)  # revealed: None
```

An ID of `0` is falsy but can refer to a row, so a falsy ID does not narrow the foreign key:

```py
def _(book: Book):
    if not book.shelf_id:  # error: [truthiness-test-of-none-union]
        reveal_type(book.shelf)  # revealed: Shelf | None
```

The check can be part of a boolean expression:

```py
def _(book: Book):
    if book.shelf_id is not None and book.shelf.label:
        reveal_type(book.shelf)  # revealed: Shelf

    if book.shelf_id is None or book.shelf.label:
        reveal_type(book.shelf)  # revealed: Shelf | None
    else:
        reveal_type(book.shelf)  # revealed: Shelf
```

An `assert` or an early return narrows the code that follows:

```py
def _(book: Book):
    assert book.shelf_id is not None
    reveal_type(book.shelf)  # revealed: Shelf

def _(book: Book) -> str:
    if book.shelf_id is None:
        return ""
    reveal_type(book.shelf)  # revealed: Shelf
    return book.shelf.label
```

The check also narrows the foreign key of `self` in a method of the model:

```py
class Novel(models.Model):
    shelf = models.ForeignKey(Shelf, null=True, on_delete=models.SET_NULL)

    def shelf_label(self) -> str:
        if self.shelf_id is not None:
            reveal_type(self.shelf)  # revealed: Shelf
            return self.shelf.label
        return ""
```

A nullable `OneToOneField` is a foreign key too, and a model inherits the foreign keys of its
parent:

```py
class Edition(Book):
    cover = models.OneToOneField(Shelf, null=True, on_delete=models.SET_NULL, related_name="+")

def _(edition: Edition):
    if edition.cover_id is not None:
        reveal_type(edition.cover)  # revealed: Shelf
    if edition.shelf_id is not None:
        reveal_type(edition.shelf)  # revealed: Shelf
```

The result of the check can be stored in a variable first:

```py
def _(book: Book):
    has_shelf = book.shelf_id is not None
    if has_shelf:
        reveal_type(book.shelf)  # revealed: Shelf
    if not has_shelf:
        reveal_type(book.shelf)  # revealed: None

def _(book: Book):
    no_shelf = book.shelf_id is None
    if no_shelf:
        reveal_type(book.shelf)  # revealed: None
    else:
        reveal_type(book.shelf)  # revealed: Shelf
```

A variable that holds the ID itself does not narrow the foreign key, and neither does a comparison
with `==` or `!=`:

```py
def _(book: Book):
    shelf_id = book.shelf_id
    if shelf_id is not None:
        reveal_type(book.shelf)  # revealed: Shelf | None

    if book.shelf_id != None:
        reveal_type(book.shelf)  # revealed: Shelf | None

    if book.shelf_id == 1:
        reveal_type(book.shelf)  # revealed: Shelf | None
```

Assigning to the instance or to the foreign key ends the narrowing. The foreign key is a descriptor,
so after an assignment to it, we use its declared type:

```py
def _(book: Book, other: Book):
    if book.shelf_id is not None:
        book = other
        reveal_type(book.shelf)  # revealed: Shelf | None

def _(book: Book):
    if book.shelf_id is not None:
        book.shelf = None
        reveal_type(book.shelf)  # revealed: Shelf | None
```

We do not notice an assignment to the ID after the check:

```py
def _(book: Book):
    if book.shelf_id is not None:
        book.shelf_id = None
        # TODO: Should be `Shelf | None`.
        reveal_type(book.shelf)  # revealed: Shelf
```

## References to models of another app

A reference of the form `"<app label>.<model>"` names a model of another app. The label is by
default the name of the app's package, and we look for the model in the `models` module of a package
with that name:

`library/__init__.py`:

```py
```

`library/models.py`:

```py
from django.db import models

class Author(models.Model):
    name = models.CharField(max_length=100)

class Book(models.Model):
    isbn = models.CharField(max_length=13, primary_key=True)
```

`reviews/__init__.py`:

```py
```

`reviews/models.py`:

```py
from django.db import models

class Review(models.Model):
    book = models.ForeignKey("library.Book", on_delete=models.CASCADE)
    editor = models.ForeignKey("library.Author", null=True, on_delete=models.SET_NULL)
    readers = models.ManyToManyField("library.Author")

def _(review: Review):
    reveal_type(review.book)  # revealed: Book
    reveal_type(review.book_id)  # revealed: str
    reveal_type(review.editor)  # revealed: Author | None
    reveal_type(review.readers.get())  # revealed: Author
```

## Models looked up by name

`apps.get_model()` returns the model class for an app label and a model name, given separately or as
`"<app label>.<model>"`:

`library/__init__.py`:

```py
```

`library/models.py`:

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)
```

`report.py`:

```py
from django.apps import apps

reveal_type(apps.get_model("library", "Book"))  # revealed: type[Book]
reveal_type(apps.get_model("library.Book"))  # revealed: type[Book]

def _(label: str):
    reveal_type(apps.get_model(label))  # revealed: type[Any]
```

The registry that a migration receives holds historical models, which can differ from the models in
the source code, so its models keep the declared type:

```py
from django.db.migrations.state import StateApps

def _(state: StateApps):
    reveal_type(state.get_model("library", "Book"))  # revealed: type[Any]
```

We only find the models of the project's apps, so a model of an installed package keeps the declared
type too:

`auth_report.py`:

```py
from django.apps import apps

reveal_type(apps.get_model("auth", "User"))  # revealed: type[Any]
```

## Choices

The `choices` parameter can be used to map the actual stored values for a field to human-readable
labels. Django adds `get_<field>_display()` methods for fields that have `choices` defined:

```py
from django.db import models

class Article(models.Model):
    status = models.CharField(
        max_length=1,
        choices=[("D", "Draft"), ("P", "Published")],
    )
    visibility = models.CharField(
        name="access",
        max_length=1,
        choices=[("P", "Public"), ("R", "Private")],
    )

article = Article(status="P")

reveal_type(article.status)  # revealed: str
reveal_type(article.get_status_display())  # revealed: str
```

An explicit `name` determines the names of generated methods, overriding the assignment name:

```py
reveal_type(article.get_access_display())  # revealed: str

# A `get_<field>_display()` method for the original field name is not available.
article.get_visibility_display()  # error: [missing-attribute]
```

## Date navigation

Django adds `get_next_by_<field>()` and `get_previous_by_<field>()` methods for date fields that do
not have `null=True`. These methods return the next/previous instance of the corresponding model
when ordered by that specific date field:

```py
from django.db import models

class Article(models.Model):
    published = models.DateField()
    updated = models.DateTimeField()
    reviewed = models.DateField(null=True)

def _(article: Article):
    reveal_type(article.get_next_by_published())  # revealed: Article
    reveal_type(article.get_previous_by_published())  # revealed: Article

    # Date fields with `null=True` do not generate these methods.
    article.get_next_by_reviewed()  # error: [missing-attribute]
    article.get_previous_by_reviewed()  # error: [missing-attribute]
```

`DateTimeField` also generates these methods:

```py
def _(article: Article):
    reveal_type(article.get_next_by_updated())  # revealed: Article
    reveal_type(article.get_previous_by_updated())  # revealed: Article
```

Keyword arguments filter the candidate instances:

```py
def _(article: Article):
    reveal_type(article.get_next_by_published(published__year=2026))  # revealed: Article
    reveal_type(article.get_previous_by_published(published__year=2026))  # revealed: Article
```

These methods return `Self`, so when invoked on subclasses, we get back instances of the subclass:

```py
class NewsArticle(Article):
    pass

def _(article: NewsArticle):
    reveal_type(article.get_next_by_published())  # revealed: NewsArticle
    reveal_type(article.get_previous_by_published())  # revealed: NewsArticle
    reveal_type(Article.get_next_by_published(article))  # revealed: NewsArticle
    reveal_type(Article.get_previous_by_published(article))  # revealed: NewsArticle
```

## Abstract models

### Basic

An abstract model provides reusable fields without creating its own database table. Its concrete
subclasses receive an integer primary key unless they declare or inherit an explicit primary key.

```py
from django.db import models

class AbstractBook(models.Model):
    title = models.CharField(max_length=200)

    class Meta:
        abstract = True

class Ebook(AbstractBook):
    file_size = models.IntegerField()

class Book(AbstractBook):
    isbn = models.CharField(max_length=13, primary_key=True)

def _(abstract: AbstractBook, ebook: Ebook, book: Book):
    # Abstract models do not have an automatic primary key.
    abstract.id  # error: [missing-attribute]

    # Concrete models without an explicit primary key have an automatic integer key.
    reveal_type(ebook.id)  # revealed: int
    reveal_type(ebook.pk)  # revealed: int
    reveal_type(ebook.title)  # revealed: str
    reveal_type(ebook.file_size)  # revealed: int

    # The explicit ISBN primary key prevents an automatic `id`.
    book.id  # error: [missing-attribute]
    reveal_type(book.pk)  # revealed: str
    reveal_type(book.title)  # revealed: str
    reveal_type(book.isbn)  # revealed: str
```

### Annotated abstract setting

A bare annotation for `Meta.abstract` does not make a model abstract.

```py
from django.db import models

class Book(models.Model):
    class Meta:
        abstract: bool

def _(book: Book):
    reveal_type(book.id)  # revealed: int
    reveal_type(book.pk)  # revealed: int
```

When an assignment follows the annotation, its value determines whether the model is abstract.

```py
class AbstractBook(models.Model):
    class Meta:
        abstract: bool
        abstract = True

def _(abstract: AbstractBook):
    abstract.id  # error: [missing-attribute]
```

### Inherited Meta configuration

Django resets an abstract model's `Meta.abstract` to `False`. A child that subclasses this `Meta` is
concrete unless it explicitly sets `abstract = True` again.

```py
from django.db import models

class AbstractBook(models.Model):
    class Meta:
        abstract = True

class NotAbstract(AbstractBook):
    class Meta(AbstractBook.Meta):
        ordering = ["id"]

class Abstract(AbstractBook):
    class Meta(AbstractBook.Meta):
        abstract = True

def _(not_abstract: NotAbstract, abstract: Abstract):
    # concrete models have an automatic primary key
    not_abstract.id

    # abstract models do not:
    abstract.id  # error: [missing-attribute]
```

Configuration inherited from an ordinary class retains its `abstract` setting.

```py
class Options:
    abstract = True

class InheritedAbstract(models.Model):
    class Meta(Options):
        pass

def _(abstract: InheritedAbstract):
    abstract.id  # error: [missing-attribute]
```

A model does not inherit the `Meta` class of a concrete parent, and can declare an unrelated one:

```py
class Book(models.Model):
    title = models.CharField(max_length=200)

    class Meta:
        ordering = ["title"]

class Ebook(Book):
    class Meta:  # ok
        ordering = ["-title"]
```

### Multiple inheritance

An abstract base without a primary key does not shadow an inherited `id`, regardless of the base
class order. Regression test for <https://github.com/astral-sh/ty/issues/4822>.

```py
from uuid import uuid4
from django.db import models

class AbstractHasTitle(models.Model):
    title = models.CharField(max_length=200)

    class Meta:
        abstract = True

class AbstractHasId(models.Model):
    id = models.UUIDField(primary_key=True, default=uuid4)

    class Meta:
        abstract = True

class Book1(AbstractHasTitle, AbstractHasId):
    pass

class Book2(AbstractHasId, AbstractHasTitle):
    pass

def _(book1: Book1, book2: Book2):
    reveal_type(book1.id)  # revealed: UUID
    reveal_type(book1.pk)  # revealed: UUID
    reveal_type(book1.title)  # revealed: str

    reveal_type(book2.id)  # revealed: UUID
    reveal_type(book2.pk)  # revealed: UUID
    reveal_type(book2.title)  # revealed: str
```

## Reverse accessors

A `OneToOneField` adds a reverse accessor that returns the single related instance. Its default name
is the lowercase source model name, without `_set`:

```py
from django.db import models

class Author(models.Model):
    pass

class Profile(models.Model):
    author = models.OneToOneField(Author, on_delete=models.CASCADE)

def _(author: Author):
    reveal_type(author.profile)  # revealed: Profile
```

A subclass of `OneToOneField` also adds a reverse accessor without `_set`:

```py
class BadgeLink(models.OneToOneField):
    pass

class Badge(models.Model):
    author = BadgeLink(Author, on_delete=models.CASCADE)

def _(author: Author):
    reveal_type(author.badge)  # revealed: Badge
```

A `related_name` on an abstract model can contain `%(class)s`, which Django replaces with the
lowercase name of each concrete subclass:

```py
class Publication(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="%(class)ss")

    class Meta:
        abstract = True

class Book(Publication):
    pass

def _(author: Author):
    reveal_type(author.books)  # revealed: RelatedManager[Book, QuerySet[Book, Book]]
    reveal_type(author.books.first())  # revealed: Book | None

    # An abstract model has no reverse accessor of its own.
    author.publications  # error: [missing-attribute]
```

A relation to `"self"` adds the reverse accessor to the model itself:

```py
class Category(models.Model):
    parent = models.ForeignKey("self", null=True, on_delete=models.SET_NULL, related_name="children")

def _(category: Category):
    reveal_type(category.children.first())  # revealed: Category | None
```

The reverse accessor's manager has the methods of the related model's default manager:

```py
class StoryQuerySet(models.QuerySet["Story"]):
    def published(self) -> "StoryQuerySet":
        return self.filter(title__startswith="A")

class Story(models.Model):
    title = models.CharField(max_length=100)
    author = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="stories")

    objects = StoryQuerySet.as_manager()

def _(author: Author):
    reveal_type(author.stories.published())  # revealed: StoryQuerySet
    reveal_type(author.stories.first())  # revealed: Story | None
```

The default manager is the first manager that the model declares, unless `Meta.default_manager_name`
names another one:

```py
class LoudManager(models.Manager):  # error: [missing-type-argument]
    def shout(self) -> str:
        return "!"

class Poem(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

    objects = models.Manager()
    loud = LoudManager()

class Essay(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

    objects = models.Manager()
    loud = LoudManager()

    class Meta:
        default_manager_name = "loud"

def _(author: Author):
    author.poem_set.shout()  # error: [missing-attribute]
    reveal_type(author.essay_set.shout())  # revealed: str
```

A model that declares no managers of its own uses the `default_manager_name` of its parent:

```py
class Speech(models.Model):
    objects = models.Manager()
    loud = LoudManager()

    class Meta:
        default_manager_name = "loud"

class Toast(Speech):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

def _(author: Author):
    reveal_type(author.toast_set.shout())  # revealed: str
```

Only the first parent model counts. `Toast` has no `default_manager_name` of its own, because a
model does not inherit the `Meta` class of a concrete parent, so its child `Roast` uses its first
manager, `objects`:

```py
class Roast(Toast):
    critic = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="roasts")

def _(author: Author):
    author.roasts.shout()  # error: [missing-attribute]
```

A model that declares a manager does not use the `default_manager_name` of its parent. Its own
managers come before the inherited ones:

```py
class Lecture(Speech):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

    quiet = models.Manager()

def _(author: Author):
    author.lecture_set.shout()  # error: [missing-attribute]
```

A model inherits the `Meta` class of an abstract parent, with its `default_manager_name`, whether or
not it declares managers of its own:

```py
class Recital(models.Model):
    objects = models.Manager()
    loud = LoudManager()

    class Meta:
        abstract = True
        default_manager_name = "loud"

class Ballad(Recital):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

class Hymn(Recital):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

    quiet = models.Manager()

def _(author: Author):
    reveal_type(author.ballad_set.shout())  # revealed: str
    reveal_type(author.hymn_set.shout())  # revealed: str
```

A member that the model declares, such as a field, a method, a manager or an annotation, keeps its
type when a reverse accessor has the same name. Django's system checks reject a reverse accessor
that clashes with a field, and one that replaces another member is a mistake:

```py
class Publisher(models.Model):
    name = models.CharField(max_length=100)
    catalogue = models.Manager()
    reviews: int

    def magazine_set(self) -> str:
        return ""

class Magazine(models.Model):
    publisher = models.ForeignKey(Publisher, on_delete=models.CASCADE)
    printer = models.ForeignKey(Publisher, on_delete=models.CASCADE, related_name="name")
    reviewer = models.ForeignKey(Publisher, on_delete=models.CASCADE, related_name="reviews")
    distributor = models.ForeignKey(Publisher, on_delete=models.CASCADE, related_name="catalogue")

reveal_type(Publisher.catalogue)  # revealed: Manager[Publisher, QuerySet[Publisher, Publisher]]

def _(publisher: Publisher):
    reveal_type(publisher.name)  # revealed: str
    reveal_type(publisher.reviews)  # revealed: int
    reveal_type(publisher.magazine_set())  # revealed: str
```

A reverse accessor can be named `objects` on a model whose managers have other names. On a model
without managers, the `objects` manager that Django adds keeps its name:

```py
class Rack(models.Model):
    items = models.Manager()

class Slot(models.Model):
    rack = models.ForeignKey(Rack, on_delete=models.CASCADE, related_name="objects")

class Crate(models.Model):
    pass

class Parcel(models.Model):
    crate = models.ForeignKey(Crate, on_delete=models.CASCADE, related_name="objects")

def _(rack: Rack):
    reveal_type(rack.objects.get())  # revealed: Slot

reveal_type(Crate.objects.get())  # revealed: Crate
```

## Reverse accessors across modules

A relation that is declared in another app also adds a reverse accessor. Django imports the `models`
module of each installed app, so we look for relations in every `models` module of the project:

`library/__init__.py`:

```py
```

`library/models.py`:

```py
from django.db import models

class Author(models.Model):
    pass
```

`shop/__init__.py`:

```py
```

`shop/models.py`:

```py
from django.db import models
from library.models import Author

class Order(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
    reviewer = models.ForeignKey("library.Author", on_delete=models.CASCADE, related_name="reviews")

def _(author: Author):
    reveal_type(author.order_set.get())  # revealed: Order
    reveal_type(author.reviews.get())  # revealed: Order
```

A model in a module that is not a `models` module only adds reverse accessors to the models of its
own module:

`shop/drafts.py`:

```py
from django.db import models
from library.models import Author

class Draft(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

def _(author: Author):
    author.draft_set  # error: [missing-attribute]
```

A `models` package counts too, with each of its submodules:

`billing/__init__.py`:

```py
```

`billing/models/__init__.py`:

```py
```

`billing/models/invoice.py`:

```py
from django.db import models
from library.models import Author

class Invoice(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
```

`library/views.py`:

```py
from library.models import Author

def _(author: Author):
    reveal_type(author.invoice_set.get())  # revealed: Invoice
```

## User model

`get_user_model()` returns the model named by the `AUTH_USER_MODEL` setting, which is
`django.contrib.auth.models.User` by default. django-stubs declares the user model as
`AbstractBaseUser`, a placeholder that a type checker is expected to refine. When we cannot read the
setting, we use `AbstractUser`, the base class of `User` and of most custom user models. Here the
project has no settings module that we can find:

```py
from django.contrib.auth import get_user_model
from django.contrib.auth.models import AbstractUser
from django.http import HttpRequest

User = get_user_model()
reveal_type(User)  # revealed: type[AbstractUser]
reveal_type(User.objects.create_user("alice"))  # revealed: AbstractUser

def _(user: AbstractUser):
    reveal_type(user.username)  # revealed: str
    reveal_type(user.is_superuser)  # revealed: bool

def _(request: HttpRequest):
    reveal_type(request.user)  # revealed: AbstractUser | AnonymousUser
    reveal_type(request.user.is_superuser)  # revealed: bool

    if request.user.is_authenticated:
        reveal_type(request.user)  # revealed: AbstractUser
```

The result of `get_user_model()` is a variable, so it cannot be used as a type. mypy reports the
same error, even with the django-stubs plugin:

```py
# error: [invalid-type-form]
def _(user: User):
    reveal_type(user)  # revealed: Unknown
```

Importing the user model for type checking only gives the annotation the real model:

```py
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from django.contrib.auth.models import User as UserModel
else:
    UserModel = get_user_model()

def _(user: UserModel):
    reveal_type(user)  # revealed: User
    reveal_type(user.username)  # revealed: str
```

## Primary key of an abstract model's subclasses

An abstract model cannot be instantiated, so a value annotated with an abstract model is always an
instance of a concrete subclass, which has a primary key. `pk` and `id` follow the primary keys of
the concrete subclasses that we find:

```py
from django.db import models

class Publication(models.Model):
    class Meta:
        abstract = True

class Book(Publication):
    pass

class Magazine(Publication):
    pass

def _(publication: Publication):
    reveal_type(publication.pk)  # revealed: int
    reveal_type(publication.id)  # revealed: int
```

`id` is only available if every concrete subclass has an automatic primary key:

```py
class Document(models.Model):
    class Meta:
        abstract = True

class Report(Document):
    pass

class Standard(Document):
    code = models.CharField(max_length=20, primary_key=True)

def _(document: Document):
    reveal_type(document.pk)  # revealed: int | str
    document.id  # error: [missing-attribute]
```

An abstract model without concrete subclasses keeps the `pk` type from the stubs:

```py
class Unused(models.Model):
    class Meta:
        abstract = True

def _(unused: Unused):
    reveal_type(unused.pk)  # revealed: Any
    unused.id  # error: [missing-attribute]
```

## Choices classes

The `choices` parameter accepts a subclass of Django's `Choices`, such as `TextChoices`:

```py
from django.db import models

class Status(models.TextChoices):
    DRAFT = "D", "Draft"
    PUBLISHED = "P", "Published"

class Priority(models.IntegerChoices):
    LOW = 1, "Low"
    HIGH = 2, "High"

class Article(models.Model):
    status = models.CharField(max_length=1, choices=Status, default=Status.DRAFT)  # ok
    priority = models.IntegerField(choices=Priority, default=Priority.LOW)  # ok

def _(article: Article):
    reveal_type(article.status)  # revealed: str
    reveal_type(article.get_status_display())  # revealed: str
    reveal_type(article.get_priority_display())  # revealed: str
```

Other classes are still rejected:

```py
class Comment(models.Model):
    status = models.CharField(max_length=1, choices=int)  # error: [invalid-argument-type]
```

## Custom `QuerySet` methods on managers

`QuerySet.as_manager()` and `Manager.from_queryset()` create a manager that has a copy of the custom
`QuerySet`'s methods:

```py
from django.db import models

class ArticleQuerySet(models.QuerySet["Article"]):
    def published(self) -> "ArticleQuerySet":
        return self.filter(published=True)

class Article(models.Model):
    published = models.BooleanField(default=False)

    objects = ArticleQuerySet.as_manager()
    reviewed = models.Manager.from_queryset(ArticleQuerySet)()

reveal_type(Article.objects)  # revealed: Manager[Article, ArticleQuerySet]
reveal_type(Article.objects.all().published())  # revealed: ArticleQuerySet
reveal_type(Article.objects.published())  # revealed: ArticleQuerySet
reveal_type(Article.objects.published().published())  # revealed: ArticleQuerySet

reveal_type(Article.reviewed)  # revealed: Manager[Article, ArticleQuerySet]
reveal_type(Article.reviewed.published())  # revealed: ArticleQuerySet
```

The class that `from_queryset()` returns can also be used as a base class:

```py
class ArticleManager(models.Manager.from_queryset(ArticleQuerySet)):
    def drafts(self) -> ArticleQuerySet:
        return self.filter(published=False)

class Draft(models.Model):
    published = models.BooleanField(default=False)

    objects = ArticleManager()

reveal_type(Draft.objects.drafts())  # revealed: ArticleQuerySet
reveal_type(Draft.objects.published())  # revealed: ArticleQuerySet
```

The manager only has the public methods that the custom `QuerySet` class adds. Django does not copy
private methods, other attributes, or the methods of `QuerySet` that only make sense on a
`QuerySet`, such as `delete()`:

```py
class BookQuerySet(models.QuerySet["Book"]):
    default_ordering = "title"

    def by_title(self, title: str) -> "BookQuerySet":
        return self.filter(title=title)

    def _recent(self) -> "BookQuerySet":
        return self

class Book(models.Model):
    title = models.CharField(max_length=200)

    objects = BookQuerySet.as_manager()

reveal_type(Book.objects.by_title("Example"))  # revealed: BookQuerySet
Book.objects.by_title(1)  # error: [invalid-argument-type]

Book.objects._recent()  # error: [missing-attribute]
Book.objects.default_ordering  # error: [missing-attribute]
Book.objects.delete()  # error: [missing-attribute]
```

A `create()` method that the custom `QuerySet` class defines replaces Django's, and keeps its own
parameters:

```py
from typing import Any

class ImportQuerySet(models.QuerySet["Import"]):
    def create(self, **kwargs: Any) -> "Import":
        return super().create(**{name.removeprefix("raw_"): value for name, value in kwargs.items()})

class Import(models.Model):
    title = models.CharField(max_length=200)

    objects = ImportQuerySet.as_manager()

def _():
    reveal_type(Import.objects.create(raw_title="Dune"))  # revealed: Import
```

## `values()` and `values_list()`

`values()` returns each row as a dictionary, and `values_list()` returns it as a tuple, or as a
single value with `flat=True`. The types follow the named fields:

```py
from django.db import models

class Article(models.Model):
    title = models.CharField(max_length=100)
    rating = models.IntegerField()

row = Article.objects.values("title").get()
reveal_type(row)  # revealed: <TypedDict with items 'title'>
reveal_type(row["title"])  # revealed: str
row["rating"]  # error: [invalid-typed-dict-key]

reveal_type(Article.objects.values_list("title", "rating").get())  # revealed: tuple[str, int]
reveal_type(Article.objects.values_list("title", flat=True).get())  # revealed: str
reveal_type(Article.objects.filter(rating=5).values_list("pk").get())  # revealed: tuple[int]
```

The rows keep their declared types when a field is not named by a string literal, when a name ends
in a transform such as `__length`, and for expressions passed as keyword arguments:

```py
from django.db.models.functions import Length

def _(field: str):
    reveal_type(Article.objects.values(field).get())  # revealed: dict[str, Any]
    reveal_type(Article.objects.values_list(field).get())  # revealed: Any

reveal_type(Article.objects.values("title", n=Length("title")).get())  # revealed: dict[str, Any]
reveal_type(Article.objects.values_list("title__length").get())  # revealed: Any
```

With `named=True`, each row is a named tuple:

```py
named_row = Article.objects.values_list("title", "rating", named=True).get()
reveal_type(named_row.title)  # revealed: str
reveal_type(named_row.rating)  # revealed: int
reveal_type(named_row[0])  # revealed: str
```

Django raises a `TypeError` for `flat=True` with more than one field, and for `flat=True` together
with `named=True`:

```py
Article.objects.values_list("title", "rating", flat=True)  # error: [invalid-argument-type]
Article.objects.values_list("title", flat=True, named=True)  # error: [invalid-argument-type]
```

A name can follow relations to the fields of other models. The value can be `None` when a foreign
key is nullable, or when the relation goes from the other side, because a row can have no related
object:

```py
class Writer(models.Model):
    name = models.CharField(max_length=100)

class Column(models.Model):
    title = models.CharField(max_length=100)
    writer = models.ForeignKey(Writer, on_delete=models.CASCADE, related_name="columns")
    editor = models.ForeignKey(Writer, null=True, on_delete=models.SET_NULL, related_name="edited")

column_row = Column.objects.values("title", "writer__name").get()
reveal_type(column_row["writer__name"])  # revealed: str

reveal_type(Column.objects.values_list("editor__name", flat=True).get())  # revealed: str | None
reveal_type(Writer.objects.values_list("columns__title", flat=True).get())  # revealed: str | None
```

Only Django's managers and querysets get these row types. A method with the same name on another
class keeps its declared type, as in mypy:

```py
from typing import Any

class Repository:
    def values_list(self, *fields: str, flat: bool = False, named: bool = False) -> models.QuerySet[Article, Any]:
        raise NotImplementedError

reveal_type(Repository().values_list("title", flat=True))  # revealed: QuerySet[Article, Any]
```

## Annotations

`annotate()` adds an attribute to each instance in the `QuerySet` for every keyword argument:

```py
from django.db import models
from django.db.models.functions import Length

class Article(models.Model):
    title = models.CharField(max_length=100)

article = Article.objects.annotate(title_length=Length("title")).get()

reveal_type(article.title)  # revealed: str
article.word_count  # error: [missing-attribute]
```

We do not infer the type of an annotation's expression, so the attribute has the type `Any`:

```py
reveal_type(article.title_length)  # revealed: Any
```

The annotated model is still the model, and the other `QuerySet` methods keep the annotations:

```py
def publish(article: Article) -> None: ...

annotated = Article.objects.annotate(title_length=Length("title")).filter(title_length=7)
for article in annotated.order_by("title"):
    publish(article)  # ok
    reveal_type(article.title_length)  # revealed: Any

reveal_type(annotated.values_list("title").get())  # revealed: tuple[str]
```

An aggregate passed as a positional argument is named after its field and its function, as in
`article__count`:

```py
from django.db.models import Count, Max

class Review(models.Model):
    article = models.ForeignKey(Article, on_delete=models.CASCADE)
    rating = models.IntegerField()

stats = Review.objects.annotate(Count("article"), Max("rating")).get()
reveal_type(stats.article__count)  # revealed: Any
reveal_type(stats.rating__max)  # revealed: Any
```

A subclass of an aggregate is named after the function of the Django aggregate that it extends:

```py
class DistinctCount(Count):
    pass

def _():
    distinct = Review.objects.annotate(DistinctCount("article")).get()
    reveal_type(distinct.article__count)  # revealed: Any
```

Django has no name for an aggregate with a `filter`, and raises a `TypeError` that asks for one, so
the instances get no attribute:

```py
from django.db.models import Q

def _():
    filtered = Review.objects.annotate(Count("article", filter=Q(rating=5))).get()
    filtered.article__count  # error: [missing-attribute]
```

A custom `QuerySet` class gets the annotations too, and keeps its own methods:

```py
class StoryQuerySet(models.QuerySet["Story"]):
    def published(self) -> "StoryQuerySet":
        return self.filter(title__startswith="A")

class Story(models.Model):
    title = models.CharField(max_length=100)

    objects = StoryQuerySet.as_manager()

story = Story.objects.annotate(title_length=Length("title")).get()
reveal_type(story.title_length)  # revealed: Any

published = Story.objects.published().annotate(title_length=Length("title")).get()
reveal_type(published.title_length)  # revealed: Any

Story.objects.annotate(title_length=Length("title")).published()  # ok
```

In a type annotation, `WithAnnotations` from django-stubs-ext names a model with annotations. The
attributes and their types come from a `TypedDict`:

```py
from typing import TypedDict

from django_stubs_ext import WithAnnotations

class TitleLength(TypedDict):
    title_length: int

def _(article: WithAnnotations[Article, TitleLength]):
    reveal_type(article.title_length)  # revealed: int
    reveal_type(article.title)  # revealed: str
    article.word_count  # error: [missing-attribute]
```

A method named `annotate` on a class that is not a Django manager or queryset keeps its declared
type:

```py
from typing import Any

class Archive:
    def annotate(self, **kwargs: Any) -> models.QuerySet[Article, Article]:
        raise NotImplementedError

reveal_type(Archive().annotate(title_length=1))  # revealed: QuerySet[Article, Article]
```

## Prefetched relations

`prefetch_related()` with a `Prefetch` object that has a `to_attr` stores the related objects in a
list on each instance:

```py
from django.db import models
from django.db.models import Prefetch

class Author(models.Model):
    name = models.CharField(max_length=100)

class Book(models.Model):
    title = models.CharField(max_length=200)
    author = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="books")

author = Author.objects.prefetch_related(Prefetch("books", to_attr="book_list")).get()
reveal_type(author.book_list)  # revealed: list[Book]
reveal_type(author.name)  # revealed: str
```

The `queryset` of the `Prefetch` object can select the related objects:

```py
dune = Book.objects.filter(title="Dune")
reader = Author.objects.prefetch_related(Prefetch("books", queryset=dune, to_attr="dune_books")).get()
reveal_type(reader.dune_books)  # revealed: list[Book]
```

Without `to_attr`, Django stores the objects behind the reverse accessor, and adds no attribute:

```py
plain = Author.objects.prefetch_related(Prefetch("books")).get()
plain.book_list  # error: [missing-attribute]
```

A lookup that spans relations sets the attribute on the related objects at its end, not on the
instances of the `QuerySet`:

```py
class Chapter(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE, related_name="chapters")

def _():
    author = Author.objects.prefetch_related(Prefetch("books__chapters", to_attr="chapter_list")).get()
    author.chapter_list  # error: [missing-attribute]
```

A forward foreign key leads to a single object, which Django stores as it is, or `None` if there is
none:

```py
class Review(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE)
    editor = models.ForeignKey(Author, null=True, on_delete=models.SET_NULL, related_name="+")

def _():
    review = Review.objects.prefetch_related(
        Prefetch("book", to_attr="reviewed_book"),
        Prefetch("editor", to_attr="review_editor"),
    ).get()
    reveal_type(review.reviewed_book)  # revealed: Book
    reveal_type(review.review_editor)  # revealed: Author | None
```

The `queryset` of a `Prefetch` object can leave the related object out, which Django stores as
`None`. The reverse accessor of a `OneToOneField`, and the link from a parent model to a child
model, can also have no related object. The link from a child model to its parent always has one,
and a many-to-many field leads to a list. We do not know what an unknown relation leads to:

```py
class Tag(models.Model):
    pass

class Volume(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
    tags = models.ManyToManyField(Tag)

class Jacket(models.Model):
    volume = models.OneToOneField(Volume, on_delete=models.CASCADE)

class Audiobook(Volume):
    pass

def _():
    volume = Volume.objects.prefetch_related(
        Prefetch("author", queryset=Author.objects.filter(name="Ann"), to_attr="filtered_author"),
        Prefetch("jacket", to_attr="volume_jacket"),
        Prefetch("audiobook", to_attr="volume_audiobook"),
        Prefetch("tags", to_attr="tag_list"),
        Prefetch("unknown", to_attr="mystery"),
    ).get()
    reveal_type(volume.filtered_author)  # revealed: Author | None
    reveal_type(volume.volume_jacket)  # revealed: Jacket | None
    reveal_type(volume.volume_audiobook)  # revealed: Audiobook | None
    reveal_type(volume.tag_list)  # revealed: list[Tag]
    reveal_type(volume.mystery)  # revealed: Any

    audiobook = Audiobook.objects.prefetch_related(Prefetch("volume_ptr", to_attr="original")).get()
    reveal_type(audiobook.original)  # revealed: Volume
```

## Field assignments

django-stubs declares the types that a field accepts on assignment. They are wider than the field's
value type, because Django converts the value when it saves the instance:

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)
    pages = models.IntegerField()

def _(book: Book):
    book.title = 3  # ok
    book.pages = "300"  # ok
    book.title = None  # error: [invalid-assignment]
    book.pages = None  # error: [invalid-assignment]
```

The automatic primary key accepts a string for the same reason:

```py
def _(book: Book):
    book.id = "1"  # ok
```

A field also accepts a query expression, which Django turns into SQL when it saves the instance:

```py
def _(book: Book):
    book.pages = models.F("pages") + 1  # ok
    book.title = models.Value("Dune")  # ok
```

`update()` and `aupdate()` set the fields in the database, so their keywords accept the same values
as an assignment, including expressions:

```py
def _():
    Book.objects.update(title=models.F("title"), pages=models.F("pages") + 1)  # ok
    Book.objects.filter(pages=0).update(title=models.Value("Untitled"))  # ok
    Book.objects.update(title=None)  # error: [invalid-argument-type]

async def _():
    await Book.objects.aupdate(pages=models.F("pages") + 1)  # ok
```

The constructor and `create()` store the value on the instance as an assignment does, and Django
resolves an expression when it inserts the row. An expression that refers to a column, such as
`F("pages")`, raises a `ValueError` at that point, because the row does not exist yet, but we do not
distinguish such expressions from the others:

```py
def _():
    Book(title=models.Value("Dune"), pages=models.Value(412))  # ok
    Book.objects.create(title=models.Value("Dune"), pages=412)  # ok
    Book(pages=None)  # error: [invalid-argument-type]
```

A foreign key is different. The constructor and `create()` assign it through Django's relation
descriptor, which raises a `ValueError` for a value that is neither a model instance nor `None`.
`update()` writes the column directly, so it accepts an expression:

```py
class Author(models.Model):
    name = models.CharField(max_length=100)

class Novel(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

def _(author: Author):
    Novel(author=author)  # ok
    Novel(author=models.F("author"))  # error: [invalid-argument-type]
    Novel.objects.create(author=models.Value(1))  # error: [invalid-argument-type]
    Novel.objects.update(author=models.F("author"))  # ok
```

The `<field>_id` keyword of a foreign key accepts any value:

```py
def _():
    Novel(author_id=models.F("author_id"))  # ok
    Novel.objects.update(author_id=models.F("author_id"))  # ok
```

## Fields that convert their values

A field class gives its values their Python type in `from_db_value()` and `to_python()`. When a
subclass of a Django field overrides them without type arguments for the field class it extends, its
values have the return type that `from_db_value()` declares. Without a declared type, as in
django-multiselectfield's `MultiSelectField`, they have the type `Any`. Any value can be assigned,
because the field converts it when it is saved:

```py
from typing import Any

from django.db import models

class ListField(models.CharField):
    def from_db_value(self, value: Any, expression: Any, connection: Any) -> list[str]:
        return value.split(",")

class ChoiceListField(models.CharField):
    def from_db_value(self, value, expression, connection):
        return value.split(",")

class Book(models.Model):
    genres = ListField(max_length=100)
    flags = ChoiceListField(max_length=100)

def _(book: Book):
    reveal_type(book.genres)  # revealed: list[str]
    reveal_type(book.flags)  # revealed: Any
    book.genres = ["fiction"]  # ok
    book.flags = ["new"]  # ok
    Book(genres=["fiction"], flags=["new"])  # ok
    Book.objects.create(genres=["fiction"])  # ok
    Book.objects.update(flags=["new"])  # ok
```

A subclass that declares the types of its values keeps them, and so does a subclass that only
changes how values are saved:

```py
class TypedListField(models.CharField[list[str], list[str]]):
    def from_db_value(self, value: Any, expression: Any, connection: Any) -> list[str]:
        return value.split(",")

class LowercaseField(models.CharField):
    def get_prep_value(self, value: Any) -> Any:
        return str(value).lower()

class Magazine(models.Model):
    topics = TypedListField(max_length=100)
    code = LowercaseField(max_length=10)

def _(magazine: Magazine):
    reveal_type(magazine.topics)  # revealed: list[str]
    reveal_type(magazine.code)  # revealed: str
    magazine.code = ["x"]  # error: [invalid-assignment]
```

Without a return type on `from_db_value()`, the values have the return type that `to_python()`
declares:

```py
class TagListField(models.CharField):
    def from_db_value(self, value, expression, connection):
        return value.split(",")

    def to_python(self, value: Any) -> list[str]:
        return str(value).split(",")

class Article(models.Model):
    tags = TagListField(max_length=100)

def _(article: Article):
    reveal_type(article.tags)  # revealed: list[str]
```

An overloaded conversion method, or one whose decorator replaces the function, has no single
declared return type, so it counts as a method without one:

```py
import functools
from typing import overload

class CodeListField(models.CharField):
    @overload
    def from_db_value(self, value: None, expression: Any, connection: Any) -> None: ...
    @overload
    def from_db_value(self, value: str, expression: Any, connection: Any) -> list[int]: ...
    def from_db_value(self, value: str | None, expression: Any, connection: Any) -> list[int] | None:
        return None if value is None else [int(code) for code in value.split(",")]

    def to_python(self, value: Any) -> list[int]:
        return [int(code) for code in str(value).split(",")]

class CachedListField(models.CharField):
    @functools.cache
    def from_db_value(self, value: Any, expression: Any, connection: Any) -> list[str]:
        return value.split(",")

class Catalog(models.Model):
    codes = CodeListField(max_length=100)
    names = CachedListField(max_length=100)

def _(catalog: Catalog):
    reveal_type(catalog.codes)  # revealed: list[int]
    reveal_type(catalog.names)  # revealed: Any
```

## Many-to-many fields

A `ManyToManyField` gives each instance a manager for the related model. The `through` attribute of
the field is the model of the intermediate table, which Django creates when none is given:

```py
from django.db import models

class Tag(models.Model):
    pass

class Book(models.Model):
    tags = models.ManyToManyField(Tag)

def _(book: Book):
    reveal_type(book.tags)  # revealed: ManyRelatedManager[Tag, Book_tags]
    reveal_type(book.tags.all())  # revealed: QuerySet[Tag, Tag]

reveal_type(Book.tags.through)  # revealed: type[Book_tags]
```

The automatically created model has a foreign key to each of the two models:

```py
def _(book: Book):
    tagging = book.tags.through.objects.get()
    reveal_type(tagging.book)  # revealed: Book
    reveal_type(tagging.tag)  # revealed: Tag
    reveal_type(tagging.tag_id)  # revealed: int
    reveal_type(tagging.pk)  # revealed: int
```

For a relation from a model to itself, the foreign keys are named `from_<model>` and `to_<model>`:

```py
class Person(models.Model):
    friends = models.ManyToManyField("self")

def _(person: Person):
    friendship = person.friends.through.objects.get()
    reveal_type(friendship.from_person)  # revealed: Person
    reveal_type(friendship.to_person)  # revealed: Person
```

The related model and the model of the intermediate table can be given as a class or as the name of
a model in the same module:

```py
class Author(models.Model):
    books = models.ManyToManyField("Novel", through="Authorship")

class Novel(models.Model):
    pass

class Authorship(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
    novel = models.ForeignKey(Novel, on_delete=models.CASCADE)

def _(author: Author):
    reveal_type(author.books)  # revealed: ManyRelatedManager[Novel, Authorship]
    reveal_type(author.books.first())  # revealed: Novel | None

reveal_type(Author.books.through)  # revealed: type[Authorship]
```

A subclass of `ManyToManyField` behaves the same way, and also cannot be assigned in the
constructor:

```py
class TagField(models.ManyToManyField):  # error: [missing-type-argument]
    pass

class Magazine(models.Model):
    tags = TagField("Tag")

def _(magazine: Magazine):
    reveal_type(magazine.tags)  # revealed: ManyRelatedManager[Tag, Magazine_tags]

reveal_type(Magazine.tags.through)  # revealed: type[Magazine_tags]

Magazine(tags=[])  # error: [unknown-argument]
```

The manager has the methods of the related model's default manager:

```py
class GenreQuerySet(models.QuerySet["Genre"]):
    def popular(self) -> "GenreQuerySet":
        return self.filter(pk__lt=10)

class Genre(models.Model):
    objects = GenreQuerySet.as_manager()

class Film(models.Model):
    genres = models.ManyToManyField(Genre)

def _(film: Film):
    reveal_type(film.genres.popular())  # revealed: GenreQuerySet
    reveal_type(film.genres.first())  # revealed: Genre | None
```

`create()` of the manager takes the keywords of the related model's constructor, and the
`through_defaults` of the intermediate model:

```py
class Label(models.Model):
    name = models.CharField(max_length=20)

class Album(models.Model):
    labels = models.ManyToManyField(Label)

def _(album: Album):
    reveal_type(album.labels.create(name="Indie", through_defaults={}))  # revealed: Label
    album.labels.create(nme="Indie")  # error: [unknown-argument]
```

The related model gets a manager for the models that refer to it, with the same intermediate model.
Its name is the `related_name`, or the lowercase name of the source model followed by `_set`:

```py
class Shelf(models.Model):
    pass

class Volume(models.Model):
    shelves = models.ManyToManyField(Shelf, related_name="volumes")
    archives = models.ManyToManyField(Shelf, related_name="+")

def _(shelf: Shelf, tag: Tag, novel: Novel):
    reveal_type(shelf.volumes)  # revealed: ManyRelatedManager[Volume, Volume_shelves]
    reveal_type(shelf.volumes.first())  # revealed: Volume | None
    reveal_type(tag.book_set)  # revealed: ManyRelatedManager[Book, Book_tags]
    reveal_type(novel.author_set)  # revealed: ManyRelatedManager[Author, Authorship]
```

A `related_name` that ends with `+` hides the relation, and a symmetrical relation from a model to
itself has no reverse accessor:

```py
def _(shelf: Shelf, person: Person):
    shelf.volume_set  # error: [missing-attribute]
    person.person_set  # error: [missing-attribute]
```

With `symmetrical=False`, a relation from a model to itself goes one way, and the model gets a
reverse accessor for the other way:

```py
class Reader(models.Model):
    follows = models.ManyToManyField("self", symmetrical=False)

def _(reader: Reader):
    reveal_type(reader.follows)  # revealed: ManyRelatedManager[Reader, Reader_follows]
    reveal_type(reader.reader_set)  # revealed: ManyRelatedManager[Reader, Reader_follows]
```

When we cannot resolve the model of the intermediate table, its type is unknown:

```py
class Course(models.Model):
    tags = models.ManyToManyField(Tag, through="Enrollment")

def _(course: Course, tag: Tag):
    reveal_type(course.tags)  # revealed: ManyRelatedManager[Tag, Any]
    reveal_type(tag.course_set)  # revealed: ManyRelatedManager[Course, Unknown]
```

## Managers

The default manager is only available on the model class. Its methods return a `QuerySet`:

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)

reveal_type(Book.objects)  # revealed: Manager[Book, QuerySet[Book, Book]]
reveal_type(Book.objects.all())  # revealed: QuerySet[Book, Book]
reveal_type(Book.objects.first())  # revealed: Book | None

def _(book: Book):
    book.objects  # error: [missing-attribute]
```

A model has no other implicit class attributes. django-stubs declares `objects` through a
`__getattr__` method on the metaclass that only accepts the name `objects`, so we report any other
name as an invalid call to that method:

```py
Book.nonexistent  # error: [invalid-attribute-access]
```

## Models defined after queries

Module-level code can query models before the module defines its other models. The later models
still take part in the relations of the earlier ones:

```py
from django.db import models

class Author(models.Model):
    name = models.CharField(max_length=100)

class Book(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

Book.objects.all()
Author.objects.all()

class Review(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE)

def _(book: Book):
    reveal_type(book.review_set.get())  # revealed: Review
```

## Reverse accessors in queries before later models

A module-level query can also go through a reverse accessor before the module defines another model.
The later model still adds its reverse accessor to the earlier one:

```py
from django.db import models

class Author(models.Model):
    name = models.CharField(max_length=100)

class Book(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

Author.objects.get().book_set.all()
Book.objects.get().review_set.all()
reveal_type(Book.objects.get().review_set.all())  # revealed: QuerySet[Review, Review]

class Review(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE)

def _(author: Author, book: Book):
    reveal_type(author.book_set.get())  # revealed: Book
    reveal_type(book.review_set.get())  # revealed: Review
```

## Managers declared on the model

A manager declared in the class body belongs to the model, like the default manager. Django binds it
to the model when it creates the class, even when the manager class does not name the model:

```py
from django.db import models

class BookManager(models.Manager):  # error: [missing-type-argument]
    pass

class Book(models.Model):
    title = models.CharField(max_length=200)

    objects = models.Manager()
    published = BookManager()

reveal_type(Book.objects)  # revealed: Manager[Book, QuerySet[Book, Book]]
reveal_type(Book.objects.get())  # revealed: Book
reveal_type(Book.objects.filter(title="Dune").first())  # revealed: Book | None
```

A subclass of `Manager` that does not name the model keeps its own class and methods. Its base class
leaves the model unknown, so the methods it inherits from `Manager` still return `Unknown`:

```py
reveal_type(Book.published)  # revealed: BookManager & Manager[Book, QuerySet[Book, Book]]

# TODO: Should be `Book`.
reveal_type(Book.published.get())  # revealed: Unknown & Book
```

A model also inherits the managers of its base classes, bound to itself:

```py
class Publication(models.Model):
    objects = models.Manager()

    class Meta:
        abstract = True

class Magazine(Publication):
    pass

reveal_type(Magazine.objects.get())  # revealed: Magazine
```

## Attributes set up by other libraries

When Django creates a model class, it calls `contribute_to_class(cls, name)` on each class attribute
that has that method, and the method decides what the attribute becomes. django-simple-history's
`HistoricalRecords` installs a manager this way. We cannot see what an object from outside Django
installs, so the attribute has the type `Any`:

```py
from typing import Any

from django.db import models

class AuditTrail:
    def contribute_to_class(self, cls: Any, name: str) -> None: ...

class Note:
    pass

class Book(models.Model):
    audit = AuditTrail()
    note = Note()

reveal_type(Book.audit)  # revealed: Any
Book.audit.filter(title="Dune")  # ok

def _(book: Book):
    reveal_type(book.audit)  # revealed: Any
    reveal_type(book.note)  # revealed: Note
```

Django only calls the method on an object that is not a class, so a class with the method, and a
method of the model itself, keep their types:

```py
class Magazine(models.Model):
    trail_class = AuditTrail

    def summary(self) -> str:
        return ""

reveal_type(Magazine.trail_class)  # revealed: <class 'AuditTrail'>

def _(magazine: Magazine):
    reveal_type(magazine.summary())  # revealed: str
```

Django looks the method up with `hasattr()`, which finds any attribute on an object whose class
defines `__getattr__`, so such an attribute has the type `Any` too:

```py
class Proxy:
    def __getattr__(self, name: str) -> Any:
        raise NotImplementedError

class Report(models.Model):
    options = Proxy()

reveal_type(Report.options)  # revealed: Any
```

A subclass of a Django class keeps its type, even when it overrides `contribute_to_class`:

```py
class AuditedManager(models.Manager["Article"]):
    def contribute_to_class(self, cls: Any, name: str) -> None:
        super().contribute_to_class(cls, name)

class Article(models.Model):
    objects = AuditedManager()

reveal_type(Article.objects.get())  # revealed: Article
```

## Lookups

The keyword arguments of `filter()` name fields of the model. Django raises `FieldError` for an
unknown name:

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)

# TODO: Should be an error.
Book.objects.filter(titel="Example")
```

## Field names in queryset methods

`order_by()`, `only()`, `defer()` and `select_related()` take field names as strings, and a name can
follow relations with `__`. Django raises an error for a name that it cannot resolve:

```py
from django.db import models

class Author(models.Model):
    name = models.CharField(max_length=100)

class Book(models.Model):
    title = models.CharField(max_length=200)
    author = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="books")

Book.objects.order_by("-title", "?", "pk", "author_id", "author__name")  # ok
Author.objects.order_by("books__title")  # ok

Book.objects.order_by("titel")  # error: [invalid-argument-type] "Cannot resolve keyword 'titel' into field"
Book.objects.order_by("-titel")  # error: [invalid-argument-type]
Book.objects.order_by("author__nme")  # error: [invalid-argument-type] "Cannot resolve keyword 'nme' into field"
```

Django no longer accepts a column name qualified with its table:

```py
Book.objects.order_by(
    "library_book.title"  # error: [invalid-argument-type] "Cannot resolve keyword 'library_book.title' into field"
)
```

`only()` and `defer()` take the names of fields:

```py
Book.objects.only("title", "author__name")  # ok
Book.objects.only("titel")  # error: [invalid-argument-type] "Book has no field named 'titel'"
Book.objects.defer("titel")  # error: [invalid-argument-type] "Book has no field named 'titel'"
```

`select_related()` only takes relations:

```py
Book.objects.select_related("author")  # ok
Book.objects.select_related("titel")  # error: [invalid-argument-type]
Book.objects.select_related("title")  # error: [invalid-argument-type] "Non-relational field given in select_related: 'title'"
```

A name that is the ID attribute of a foreign key is not a relation:

```py
Book.objects.select_related(
    "author_id"  # error: [invalid-argument-type] "Invalid field name(s) given in select_related: 'author_id'"
)
```

`values()` and `values_list()` take field names too. They also accept the names that `annotate()`
adds, and anything after a field, which can be a transform:

```py
from django.db.models import Count

Book.objects.values("title", "author__name")  # ok
Book.objects.values_list("title__length", flat=True)  # ok
Book.objects.annotate(n=Count("author")).values("n")  # ok
Book.objects.values("titel")  # error: [invalid-argument-type] "Cannot resolve keyword 'titel' into field"
Book.objects.values_list("author__nme")  # error: [invalid-argument-type] "Cannot resolve keyword 'nme' into field"
```

`values()` adds its keyword arguments to the query before it resolves the field names, so a field
name can also name one of them. We do not see the names that `**` unpacks, so we do not check the
field names of such a call:

```py
from typing import Any

from django.db.models.functions import Length

Book.objects.values("title_length", title_length=Length("title"))  # ok

def _(expressions: dict[str, Any]):
    Book.objects.values("titel", **expressions)  # ok
```

The keyword arguments of `values_list()` are options, not expressions, so they add no names:

```py
Book.objects.values_list("flat", flat=True)  # error: [invalid-argument-type] "Cannot resolve keyword 'flat' into field"
Book.objects.values_list("named", named=True)  # error: [invalid-argument-type] "Cannot resolve keyword 'named' into field"
```

The `update_fields` of `save()` name fields of the model:

```py
def _(book: Book):
    book.save(update_fields=["title", "author", "author_id"])  # ok
    book.save(update_fields=["titel"])  # error: [invalid-argument-type] "'titel' is not a field of `Book` that can be updated"

async def _(book: Book):
    await book.asave(update_fields=["titel"])  # error: [invalid-argument-type]
```

Django does not update a primary key, under any of its names:

```py
def _(book: Book):
    book.save(update_fields=["pk"])  # error: [invalid-argument-type]
    book.save(update_fields=["id"])  # error: [invalid-argument-type]
```

That includes a field with `primary_key=True`, the ID attribute of a foreign key that is the primary
key, the link to a concrete parent, and a primary key that the model inherits from an abstract or a
concrete parent:

```py
class Edition(models.Model):
    isbn = models.CharField(max_length=13, primary_key=True)
    title = models.CharField(max_length=200)

class Jacket(models.Model):
    edition = models.OneToOneField(Edition, on_delete=models.CASCADE, primary_key=True)
    color = models.CharField(max_length=10)

class DigitalEdition(Edition):
    size = models.IntegerField()

class Coded(models.Model):
    code = models.CharField(max_length=5, primary_key=True)

    class Meta:
        abstract = True

class Product(Coded):
    label = models.CharField(max_length=10)

def _(edition: Edition, jacket: Jacket, digital: DigitalEdition, product: Product):
    edition.save(update_fields=["title"])  # ok
    edition.save(update_fields=["isbn"])  # error: [invalid-argument-type]

    jacket.save(update_fields=["color"])  # ok
    jacket.save(update_fields=["edition"])  # error: [invalid-argument-type]
    jacket.save(update_fields=["edition_id"])  # error: [invalid-argument-type]

    digital.save(update_fields=["title", "size"])  # ok
    digital.save(update_fields=["edition_ptr"])  # error: [invalid-argument-type]
    digital.save(update_fields=["edition_ptr_id"])  # error: [invalid-argument-type]
    digital.save(update_fields=["isbn"])  # error: [invalid-argument-type]

    product.save(update_fields=["label"])  # ok
    product.save(update_fields=["code"])  # error: [invalid-argument-type]
```

`update()` takes the new field values as keywords, which are checked like the keywords of the
constructor:

```py
reveal_type(Book.objects.filter(title="Dune").update(title="Dune Messiah"))  # revealed: int
Book.objects.update(titel="Dune")  # error: [unknown-argument]

async def _():
    await Book.objects.aupdate(titel="Dune")  # error: [unknown-argument]
```

We only check models that the project defines. An installed app can add relations to its own models
and to other apps' models, which we might not see:

```py
from django.contrib.auth.models import User

User.objects.order_by("nickname")  # ok
```

## Field names of relations

The field names of queryset methods can name the relations of a model. In this section, `Book` has a
foreign key, a relation from another model's foreign key, a one-to-one relation from another model,
and a relation from another model's many-to-many field:

```py
from django.db import models

class Author(models.Model):
    name = models.CharField(max_length=100)

class Book(models.Model):
    title = models.CharField(max_length=200)
    author = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="books")

class Review(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE, related_name="reviews", related_query_name="review")
    rating = models.IntegerField()

class Cover(models.Model):
    book = models.OneToOneField(Book, on_delete=models.CASCADE)

class Shelf(models.Model):
    books = models.ManyToManyField(Book)
```

A relation from another model is named in queries by the `related_query_name` of its field, if it
has one, rather than by its `related_name`:

```py
Book.objects.order_by("review__rating")  # ok
Book.objects.order_by("reviews__rating")  # error: [invalid-argument-type] "Cannot resolve keyword 'reviews' into field"
```

`select_related()` also follows a one-to-one relation from another model, but not a relation that
can lead to several rows, such as a many-to-many field or a foreign key from another model. The
error lists the relations that it can follow on the model, or `(none)`:

```py
Book.objects.select_related("cover")  # ok

# error: [invalid-argument-type] "Invalid field name(s) given in select_related: 'reviews'. Choices are: author, cover"
Book.objects.select_related("reviews")
# error: [invalid-argument-type] "Invalid field name(s) given in select_related: 'books'. Choices are: (none)"
Shelf.objects.select_related("books")
# error: [invalid-argument-type] "Non-relational field given in select_related: 'name'. Choices are: (none)"
Author.objects.select_related("name")
```

The `update_fields` of `save()` can name a foreign key, but Django only updates the fields that hold
a value and the forward relations to a single row. It rejects a path, a many-to-many field and a
relation from another model. The names can also be given as a tuple or a set:

```py
def _(book: Book, shelf: Shelf):
    book.save(update_fields=["author"])  # ok
    book.save(update_fields=("author__name",))  # error: [invalid-argument-type]
    book.save(update_fields={"cover"})  # error: [invalid-argument-type] "'cover' is not a field of `Book` that can be updated"
    shelf.save(update_fields=["books"])  # error: [invalid-argument-type] "'books' is not a field of `Shelf` that can be updated"
```

## Field names that we cannot all see

We do not check the field names of a model that a relation might refer to without us knowing it. A
reference to a model of an app that we cannot find might name the model, and so might a
`related_name` with `%(app_label)s`, because we do not know the label of an app:

```py
from django.db import models

class Author(models.Model):
    name = models.CharField(max_length=100)

class Note(models.Model):
    author = models.ForeignKey("elsewhere.Author", on_delete=models.CASCADE)

class Editor(models.Model):
    name = models.CharField(max_length=100)

class Publication(models.Model):
    editor = models.ForeignKey(Editor, on_delete=models.CASCADE, related_name="%(app_label)s_%(class)ss")

    class Meta:
        abstract = True

class Book(Publication):
    pass

def _():
    Author.objects.order_by("note")  # ok
    Editor.objects.order_by("library_books")  # ok
```

A base class whose type we do not know can add fields too:

```py
from typing import Any

def mixin() -> Any: ...

class Tracked(mixin(), models.Model):
    title = models.CharField(max_length=200)

def _():
    Tracked.objects.order_by("tracked_at")  # ok
```

## Field names in custom querysets

A custom manager or `QuerySet` class can add names to its queries that are not part of its type, for
example with `annotate()` in `get_queryset()`, so we do not check the field names of its queries:

```py
from django.db import models

class StoryQuerySet(models.QuerySet["Story"]):
    def with_rating(self) -> "StoryQuerySet":
        return self.order_by("rating")  # ok

class Story(models.Model):
    title = models.CharField(max_length=200)

    objects = StoryQuerySet.as_manager()

Story.objects.order_by("rating")  # ok
```

Nor do we check a method of the project that overrides one of Django's methods, because it can
accept other names:

```py
from typing import Any, Self

class LabelQuerySet(models.QuerySet["Label"]):
    def order_by(self, *field_names: Any) -> Self:
        return super().order_by(*[name.removeprefix("by_") for name in field_names])

class Label(models.Model):
    text = models.CharField(max_length=100)

    objects = LabelQuerySet.as_manager()

Label.objects.all().order_by("by_text")  # ok
```

## Model forms

The nested `Meta` class of a form configures it, and a subclass can replace it:

```py
from django import forms
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)

class BookForm(forms.ModelForm[Book]):
    class Meta:
        model = Book
        fields = ["title"]

class FullBookForm(BookForm):
    class Meta:  # ok
        model = Book
        fields = "__all__"
```

The nested `Meta` class of other classes is still checked:

```py
class Base:
    class Meta:
        pass

class Derived(Base):
    class Meta:  # error: [incompatible-attribute-override]
        pass
```

## REST framework serializers

Django REST framework configures serializers with a nested `Meta` class too, and a subclass can
replace it:

```py
from django.db import models
from rest_framework import serializers

class Book(models.Model):
    title = models.CharField(max_length=200)

class BookSerializer(serializers.ModelSerializer[Book]):
    class Meta:  # ok
        model = Book
        fields = ["title"]

class FullBookSerializer(BookSerializer):
    class Meta:  # ok
        model = Book
        fields = "__all__"
```

## Model exceptions

Every model has its own `DoesNotExist` exception, a subclass of `ObjectDoesNotExist`, and its own
`MultipleObjectsReturned` exception:

```py
from django.core.exceptions import MultipleObjectsReturned, ObjectDoesNotExist
from django.db import models

class Book(models.Model):
    pass

class Author(models.Model):
    pass

reveal_type(Book.DoesNotExist)  # revealed: <class 'DoesNotExist'>
reveal_type(Book.MultipleObjectsReturned)  # revealed: <class 'MultipleObjectsReturned'>

def _(book: Book):
    reveal_type(book.DoesNotExist)  # revealed: <class 'DoesNotExist'>

try:
    Book.objects.get(pk=1)
except Book.DoesNotExist as error:
    reveal_type(error)  # revealed: DoesNotExist
```

The exceptions of different models are different classes:

```py
def _(not_found: Book.DoesNotExist, duplicate: Book.MultipleObjectsReturned):
    error: ObjectDoesNotExist = not_found  # ok
    other: Author.DoesNotExist = not_found  # error: [invalid-assignment]
    base: MultipleObjectsReturned = duplicate  # ok
```

A model that inherits from a concrete model has exceptions that inherit from the parent's. An
abstract model has no exceptions of its own:

```py
class Ebook(Book):
    pass

class Publication(models.Model):
    class Meta:
        abstract = True

def _(not_found: Ebook.DoesNotExist):
    error: Book.DoesNotExist = not_found  # ok

reveal_type(Publication.DoesNotExist)  # revealed: type[ObjectDoesNotExist]
```

## Multi-table inheritance

### Parent link

A model that inherits from a concrete model gets its own database table. Django adds a
`<parent>_ptr` field to the child, a `OneToOneField` that links to the parent model, where
`<parent>` is the lowercase name of the parent model. The `<parent>_ptr_id` attribute contains the
parent's primary-key value.

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)

class AudioBook(Book):
    minutes = models.IntegerField()

def _(book: Book, audio_book: AudioBook):
    reveal_type(audio_book.book_ptr)  # revealed: Book
    reveal_type(audio_book.book_ptr_id)  # revealed: int

    # The parent model has no link to itself, and the name must match the parent model.
    book.book_ptr  # error: [missing-attribute]
    audio_book.author_ptr  # error: [missing-attribute]
    audio_book.audio_book_ptr  # error: [missing-attribute]
```

A model inherits the parent links of its parent.

```py
class AbridgedAudioBook(AudioBook):
    pass

def _(abridged: AbridgedAudioBook):
    reveal_type(abridged.audiobook_ptr)  # revealed: AudioBook
    reveal_type(abridged.audiobook_ptr_id)  # revealed: int
    reveal_type(abridged.book_ptr)  # revealed: Book
    reveal_type(abridged.book_ptr_id)  # revealed: int
```

### Custom primary key

The type of `<parent>_ptr_id` follows the parent model's primary key.

```py
from django.db import models

class Book(models.Model):
    isbn = models.CharField(max_length=13, primary_key=True)

class Ebook(Book):
    pass

def _(ebook: Ebook):
    reveal_type(ebook.book_ptr)  # revealed: Book
    reveal_type(ebook.book_ptr_id)  # revealed: str
```

### Multiple parents

A model with several concrete parents has a parent link for each of them.

```py
from django.db import models

class Book(models.Model):
    isbn = models.CharField(max_length=13, primary_key=True)

class Product(models.Model):
    price = models.IntegerField()

class BookProduct(Product, Book):
    pass

def _(book_product: BookProduct):
    reveal_type(book_product.product_ptr)  # revealed: Product
    reveal_type(book_product.product_ptr_id)  # revealed: int
    reveal_type(book_product.book_ptr)  # revealed: Book
    reveal_type(book_product.book_ptr_id)  # revealed: str
```

### Abstract parents

An abstract model has no database table, so Django does not add a parent link for it.

```py
from django.db import models

class AbstractBook(models.Model):
    title = models.CharField(max_length=200)

    class Meta:
        abstract = True

class Book(AbstractBook):
    pass

def _(book: Book):
    book.abstractbook_ptr  # error: [missing-attribute]
```

An abstract model that inherits from a concrete model passes the parent link on to its children.

```py
class AbstractEbook(Book):
    class Meta:
        abstract = True

class Ebook(AbstractEbook):
    pass

def _(ebook: Ebook):
    reveal_type(ebook.book_ptr)  # revealed: Book
    reveal_type(ebook.book_ptr_id)  # revealed: int
    ebook.abstractebook_ptr  # error: [missing-attribute]
```

### Proxy models

A proxy model uses the database table of its parent, so Django does not add a parent link for it.

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)

class BookProxy(Book):
    class Meta:
        proxy = True

def _(proxy: BookProxy):
    proxy.book_ptr  # error: [missing-attribute]
    proxy.book_ptr_id  # error: [missing-attribute]
```

A foreign key to a proxy model returns the proxy model, but accepts any instance of the concrete
model, because Django only checks the concrete model:

```py
class Reader(models.Model):
    favourite = models.ForeignKey(BookProxy, on_delete=models.CASCADE)
    wish = models.ForeignKey("BookProxy", null=True, on_delete=models.SET_NULL, related_name="wishers")

def _(reader: Reader, book: Book):
    reveal_type(reader.favourite)  # revealed: BookProxy
    reveal_type(reader.wish)  # revealed: BookProxy | None

    reader.favourite = book  # ok
    reader.wish = book  # ok
    Reader(favourite=book, wish=book)  # ok
```

Django adds the reverse accessor of such a relation to the concrete model, and the proxy model
inherits it:

```py
def _(book: Book, proxy: BookProxy):
    reveal_type(book.wishers.get())  # revealed: Reader
    reveal_type(proxy.wishers.get())  # revealed: Reader
```

This also holds for a relation to a proxy of a proxy model, and for a many-to-many field:

```py
class BookProxyProxy(BookProxy):
    class Meta:
        proxy = True

class Collector(models.Model):
    pick = models.ForeignKey(BookProxyProxy, on_delete=models.CASCADE, related_name="pickers")
    shelf = models.ManyToManyField(BookProxy, related_name="shelvers")

def _(book: Book, proxy: BookProxy, proxy_proxy: BookProxyProxy):
    reveal_type(book.pickers.get())  # revealed: Collector
    reveal_type(proxy_proxy.pickers.get())  # revealed: Collector
    reveal_type(book.shelvers.get())  # revealed: Collector
    reveal_type(proxy.shelvers.get())  # revealed: Collector
```

A concrete child of a proxy model links to the proxy's concrete parent.

```py
class Ebook(BookProxy):
    pass

def _(ebook: Ebook):
    reveal_type(ebook.book_ptr)  # revealed: Book
    ebook.bookproxy_ptr  # error: [missing-attribute]
```

A proxy model inherits the parent links of its parent.

```py
class EbookProxy(Ebook):
    class Meta:
        proxy = True

def _(proxy: EbookProxy):
    reveal_type(proxy.book_ptr)  # revealed: Book
    proxy.ebook_ptr  # error: [missing-attribute]
```

Configuration inherited from an ordinary class retains its `proxy` setting.

```py
class Options:
    proxy = True

class InheritedProxy(Book):
    class Meta(Options):
        pass

def _(proxy: InheritedProxy):
    proxy.book_ptr  # error: [missing-attribute]
```

### Explicit parent link

A `OneToOneField` declared with `parent_link=True` replaces the automatic `<parent>_ptr` field.

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)

class Ebook(Book):
    book = models.OneToOneField(Book, on_delete=models.CASCADE, parent_link=True, primary_key=True)

def _(ebook: Ebook):
    reveal_type(ebook.book)  # revealed: Book
    reveal_type(ebook.book_id)  # revealed: int
    ebook.book_ptr  # error: [missing-attribute]
    ebook.book_ptr_id  # error: [missing-attribute]
```

Children inherit the explicit parent link instead of an automatic one.

```py
class PdfEbook(Ebook):
    pass

def _(pdf_ebook: PdfEbook):
    reveal_type(pdf_ebook.ebook_ptr)  # revealed: Ebook
    reveal_type(pdf_ebook.book)  # revealed: Book
    pdf_ebook.book_ptr  # error: [missing-attribute]
```

An explicit parent link only replaces the automatic field for the model that it relates to.

```py
class SignedPdfEbook(PdfEbook):
    pdf = models.OneToOneField(PdfEbook, on_delete=models.CASCADE, parent_link=True, primary_key=True)

def _(signed: SignedPdfEbook):
    reveal_type(signed.pdf)  # revealed: PdfEbook
    reveal_type(signed.ebook_ptr)  # revealed: Ebook
    signed.pdfebook_ptr  # error: [missing-attribute]
```

A model with several concrete parents keeps the automatic parent links to the other parents.

```py
class Product(models.Model):
    price = models.IntegerField()

class BookProduct(Product, Book):
    book = models.OneToOneField(Book, on_delete=models.CASCADE, parent_link=True)

def _(book_product: BookProduct):
    reveal_type(book_product.book)  # revealed: Book
    book_product.book_ptr  # error: [missing-attribute]
    reveal_type(book_product.product_ptr)  # revealed: Product
```

### Child accessor

The parent model has a reverse accessor named after the child model:

```py
from django.db import models

class Book(models.Model):
    pass

class Ebook(Book):
    pass

def _(book: Book):
    reveal_type(book.ebook)  # revealed: Ebook
```

A proxy model uses the table of its parent, so the parent has no accessor for it:

```py
class BookProxy(Book):
    class Meta:
        proxy = True

def _(book: Book):
    book.bookproxy  # error: [missing-attribute]
```

A relation to a child model adds the reverse accessor to the child, which has its own table, and not
to the parent:

```py
class Buyer(models.Model):
    ebook = models.ForeignKey(Ebook, on_delete=models.CASCADE, related_name="buyers")

def _(book: Book, ebook: Ebook):
    reveal_type(ebook.buyers.get())  # revealed: Buyer
    book.buyers  # error: [missing-attribute]
```

## Choices attributes

A `Choices` class has `choices`, `values` and `labels` attributes, and each member has a `label`:

```py
from django.db import models

class Status(models.TextChoices):
    DRAFT = "D", "Draft"
    PUBLISHED = "P", "Published"

reveal_type(Status.choices)  # revealed: list[tuple[str, str | _StrPromise]]
reveal_type(Status.values)  # revealed: list[str]
reveal_type(Status.DRAFT.label)  # revealed: str | _StrPromise
```

## Settings

`django.conf.settings` gives access to the settings of the project. Django's own settings have the
types of their defaults in `django.conf.global_settings`:

```py
from django.conf import settings

reveal_type(settings.DEBUG)  # revealed: bool
reveal_type(settings.INSTALLED_APPS)  # revealed: list[str]
```

Without a settings module that we can find (see below), any other setting has the type `Any`:

```py
reveal_type(settings.PAYMENT_PROVIDER)  # revealed: Any
settings.NOT_A_SETTING  # ok
```

## Project settings

Django reads the settings module that the `DJANGO_SETTINGS_MODULE` environment variable names. We
use the default that the project's `manage.py` sets. We read the settings module without running it:

`manage.py`:

```py
import os
import sys

def main() -> None:
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
    from django.core.management import execute_from_command_line

    execute_from_command_line(sys.argv)
```

`mysite/__init__.py`:

```py
```

`mysite/base.py`:

```py
PAYMENT_PROVIDER = "stripe"
DEBUG = False
```

`mysite/settings.py`:

```py
from .base import *

DEBUG = True
RETRY_LIMIT = 3
```

Settings have the types of their values in the settings module, or else of Django's defaults. Django
raises `AttributeError` for a setting that is defined in neither:

`app.py`:

```py
from django.conf import settings

reveal_type(settings.RETRY_LIMIT)  # revealed: int
reveal_type(settings.PAYMENT_PROVIDER)  # revealed: str
reveal_type(settings.DEBUG)  # revealed: bool
reveal_type(settings.INSTALLED_APPS)  # revealed: list[str]

settings.NOT_A_SETTING  # error: [invalid-attribute-access]
```

## Settings that we cannot fully see

A settings module can define settings that we cannot see, for example through a star import of a
module that we cannot find. We do not report unknown settings then:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
try:
    from .local import *  # error: [import-not-found]
except ImportError:
    pass

RETRY_LIMIT = 3
```

`app.py`:

```py
from django.conf import settings

reveal_type(settings.RETRY_LIMIT)  # revealed: int
reveal_type(settings.NOT_A_SETTING)  # revealed: Any
```

## Settings defined at runtime

A settings module can also define settings in ways that we cannot follow. We do not report unknown
settings then.

### Through a module `__getattr__`

A module-level `__getattr__` function can return any setting:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
def __getattr__(name: str) -> int:
    return 0

RETRY_LIMIT = 3
```

`app.py`:

```py
from django.conf import settings

reveal_type(settings.RETRY_LIMIT)  # revealed: int
reveal_type(settings.NOT_A_SETTING)  # revealed: Any
```

### Through `split_settings`

`django-split-settings` executes other files in the namespace of the settings module:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`split_settings/__init__.py`:

```py
```

`split_settings/tools.py`:

```py
def include(*files: str) -> None: ...
```

`mysite/settings.py`:

```py
from split_settings.tools import include

include("components/base.py")
RETRY_LIMIT = 3
```

`app.py`:

```py
from django.conf import settings

reveal_type(settings.RETRY_LIMIT)  # revealed: int
reveal_type(settings.NOT_A_SETTING)  # revealed: Any
```

### Through `setattr`

`setattr()` on the settings module defines a setting by name:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
import sys

setattr(sys.modules[__name__], "RETRY_LIMIT", 3)
```

`app.py`:

```py
from django.conf import settings

reveal_type(settings.RETRY_LIMIT)  # revealed: Any
```

### Through an alias of `globals`

Calling `globals` under another name also defines settings:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
g = globals
g()["RETRY_LIMIT"] = 3
```

`app.py`:

```py
from django.conf import settings

reveal_type(settings.RETRY_LIMIT)  # revealed: Any
```

### Through `builtins.globals()`

So does calling `globals` through the `builtins` module:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
import builtins

builtins.globals()["RETRY_LIMIT"] = 3
```

`app.py`:

```py
from django.conf import settings

reveal_type(settings.RETRY_LIMIT)  # revealed: Any
```

### A module whose name only starts with `split_settings`

We stay silent for the `split_settings` package, which includes other files at runtime, but not for
another module whose name happens to start the same way:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`split_settings_helpers.py`:

```py
```

`mysite/settings.py`:

```py
import split_settings_helpers

RETRY_LIMIT = 3
```

`app.py`:

```py
from django.conf import settings

reveal_type(settings.RETRY_LIMIT)  # revealed: int
settings.NOT_A_SETTING  # error: [invalid-attribute-access]
```

## User model from settings

### Default user model

With a settings module that does not set `AUTH_USER_MODEL`, the user model is Django's `User`:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "library"]
```

`views.py`:

```py
from django.contrib.auth import get_user_model
from django.http import HttpRequest

reveal_type(get_user_model())  # revealed: type[User]

def _(request: HttpRequest):
    if request.user.is_authenticated:
        reveal_type(request.user)  # revealed: User
```

A relation field of the project can refer to the user model through `settings.AUTH_USER_MODEL`:

`library/__init__.py`:

```py
```

`library/models.py`:

```py
from django.conf import settings
from django.db import models

class Loan(models.Model):
    reader = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.CASCADE, related_name="loans")
```

`loans.py`:

```py
from django.contrib.auth.models import User

from library.models import Loan

def _(loan: Loan, user: User):
    reveal_type(loan.reader)  # revealed: User
    reveal_type(user.loans.get())  # revealed: Loan
```

### Custom user model

`AUTH_USER_MODEL` names a custom user model as `"<app label>.<model>"`:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "accounts", "library"]
AUTH_USER_MODEL = "accounts.Member"
```

`accounts/__init__.py`:

```py
```

`accounts/models.py`:

```py
from django.contrib.auth.models import AbstractUser
from django.db import models

class Member(AbstractUser):
    nickname = models.CharField(max_length=50)
```

`views.py`:

```py
from django.contrib.auth import get_user_model
from django.http import HttpRequest

reveal_type(get_user_model())  # revealed: type[Member]

def _(request: HttpRequest):
    reveal_type(request.user)  # revealed: Member | AnonymousUser

    if request.user.is_authenticated:
        reveal_type(request.user.nickname)  # revealed: str
```

A relation to `settings.AUTH_USER_MODEL` refers to the custom user model, which gets the reverse
accessor:

`library/__init__.py`:

```py
```

`library/models.py`:

```py
from django.conf import settings
from django.db import models

class Loan(models.Model):
    reader = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.CASCADE, related_name="loans")
```

`loans.py`:

```py
from accounts.models import Member
from library.models import Loan

def _(loan: Loan, member: Member):
    reveal_type(loan.reader)  # revealed: Member
    reveal_type(member.loans.get())  # revealed: Loan
```

### User model set under a condition

When the settings module does not always set `AUTH_USER_MODEL`, we do not know the user model, and
keep `AbstractUser`:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
import os

INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "accounts"]
if os.environ.get("CUSTOM_USER") is not None:
    AUTH_USER_MODEL = "accounts.Member"
```

`accounts/__init__.py`:

```py
```

`accounts/models.py`:

```py
from django.contrib.auth.models import AbstractUser

class Member(AbstractUser):
    pass
```

`views.py`:

```py
from django.contrib.auth import get_user_model

reveal_type(get_user_model())  # revealed: type[AbstractUser]
```

### User model set in a star-imported module

The settings module can take `AUTH_USER_MODEL` from a module that it star-imports.

#### Set under a condition

When the star-imported module does not always set `AUTH_USER_MODEL`, the settings module does not
always set it either, and we keep `AbstractUser`:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/base.py`:

```py
import os

INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "accounts"]
if os.environ.get("CUSTOM_USER") is not None:
    AUTH_USER_MODEL = "accounts.Member"
```

`mysite/settings.py`:

```py
from .base import *
```

`accounts/__init__.py`:

```py
```

`accounts/models.py`:

```py
from django.contrib.auth.models import AbstractUser

class Member(AbstractUser):
    pass
```

`views.py`:

```py
from django.contrib.auth import get_user_model

reveal_type(get_user_model())  # revealed: type[AbstractUser]
```

#### Always set

When the star-imported module always sets `AUTH_USER_MODEL`, we use the model that it names:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/base.py`:

```py
INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "accounts"]
AUTH_USER_MODEL = "accounts.Member"
```

`mysite/settings.py`:

```py
from .base import *
```

`accounts/__init__.py`:

```py
```

`accounts/models.py`:

```py
from django.contrib.auth.models import AbstractUser

class Member(AbstractUser):
    pass
```

`views.py`:

```py
from django.contrib.auth import get_user_model

reveal_type(get_user_model())  # revealed: type[Member]
```

### User model that depends on a condition

Neither do we know the user model when the settings module sets `AUTH_USER_MODEL` to different
models in different branches:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
import os

INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "accounts"]
if os.environ.get("STAFF_SITE") is not None:
    AUTH_USER_MODEL = "accounts.Staff"
else:
    AUTH_USER_MODEL = "accounts.Member"
```

`accounts/__init__.py`:

```py
```

`accounts/models.py`:

```py
from django.contrib.auth.models import AbstractUser

class Member(AbstractUser):
    pass

class Staff(AbstractUser):
    pass
```

`views.py`:

```py
from django.contrib.auth import get_user_model

reveal_type(get_user_model())  # revealed: type[AbstractUser]
```

### User model in settings that we cannot fully see

When the settings module can define settings at runtime, we cannot know the user model, and keep
`AbstractUser`:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes"]
globals()["DEBUG"] = True
```

`views.py`:

```py
from django.contrib.auth import get_user_model

reveal_type(get_user_model())  # revealed: type[AbstractUser]
```

## Relations from installed apps

The models of installed apps can refer to the project's models, usually to the user model through
`settings.AUTH_USER_MODEL`. Stubs declare such fields without their arguments, so we read the
relations from the source of the apps that `INSTALLED_APPS` lists.

### Apps listed in `INSTALLED_APPS`

Here, the `Token` model of Django REST framework's `authtoken` app refers to the custom user model:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = [
    "django.contrib.auth",
    "django.contrib.contenttypes",
    "rest_framework.authtoken",
    "accounts",
]
AUTH_USER_MODEL = "accounts.Member"
```

`accounts/__init__.py`:

```py
```

`accounts/models.py`:

```py
from django.contrib.auth.models import AbstractUser

class Member(AbstractUser):
    pass
```

`views.py`:

```py
from accounts.models import Member

def _(member: Member):
    reveal_type(member.auth_token)  # revealed: Token

    # `django.contrib.admin` is not installed.
    member.logentry_set  # error: [missing-attribute]

Member.objects.order_by("auth_token__created")  # ok
```

### Models in a package

The `models` module of an installed app can be a package that imports its models from submodules:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "bookmarks"]
```

`/.venv/<path-to-site-packages>/bookmarks/__init__.py`:

```py
```

`/.venv/<path-to-site-packages>/bookmarks/models/__init__.py`:

```py
from .bookmark import Bookmark
```

`/.venv/<path-to-site-packages>/bookmarks/models/bookmark.py`:

```py
from django.conf import settings
from django.db import models

class Bookmark(models.Model):
    owner = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.CASCADE, related_name="bookmarks")
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    reveal_type(user.bookmarks.get())  # revealed: Bookmark
```

### A model of an installed app imported by the project

The module of the user model can import a model of an installed app, which makes that model one of
the models of a project `models` module too. We still read its relations from the app's source:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = [
    "django.contrib.auth",
    "django.contrib.contenttypes",
    "rest_framework.authtoken",
    "accounts",
]
AUTH_USER_MODEL = "accounts.Member"
```

`accounts/__init__.py`:

```py
```

`accounts/models.py`:

```py
from django.contrib.auth.models import AbstractUser
from rest_framework.authtoken.models import Token

class Member(AbstractUser):
    pass
```

`views.py`:

```py
from accounts.models import Member

def _(member: Member):
    reveal_type(member.auth_token)  # revealed: Token
```

### Names bound to lists

`INSTALLED_APPS` can be the sum of lists that the settings module binds to names:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
DJANGO_APPS = ["django.contrib.admin", "django.contrib.auth", "django.contrib.contenttypes"]
THIRD_PARTY_APPS = ["rest_framework.authtoken"]

INSTALLED_APPS = DJANGO_APPS + THIRD_PARTY_APPS
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    reveal_type(user.auth_token)  # revealed: Token
    reveal_type(user.logentry_set.get())  # revealed: LogEntry
```

### Sums of lists

A sum of literal lists works too:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = ["django.contrib.admin", "django.contrib.auth", "django.contrib.contenttypes"] + ["rest_framework.authtoken"]
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    reveal_type(user.auth_token)  # revealed: Token
    reveal_type(user.logentry_set.get())  # revealed: LogEntry
```

### Chains of names

A name can be bound to another name, or to a sum that contains names:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
BASE_APPS = ["django.contrib.admin", "django.contrib.auth", "django.contrib.contenttypes"]
APPS = BASE_APPS + ["rest_framework.authtoken"]

INSTALLED_APPS = APPS
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    reveal_type(user.auth_token)  # revealed: Token
    reveal_type(user.logentry_set.get())  # revealed: LogEntry
```

### Augmented assignments

`+=` adds apps to the list. We include the apps of every binding of `INSTALLED_APPS`:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = ["django.contrib.admin", "django.contrib.auth", "django.contrib.contenttypes"]
INSTALLED_APPS += ["rest_framework.authtoken"]
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    reveal_type(user.auth_token)  # revealed: Token
    reveal_type(user.logentry_set.get())  # revealed: LogEntry
```

### Star imports

The settings module can add apps to the list of a module that it star-imports:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/base.py`:

```py
INSTALLED_APPS = ["django.contrib.admin", "django.contrib.auth", "django.contrib.contenttypes"]
```

`mysite/settings.py`:

```py
from .base import *

INSTALLED_APPS += ["rest_framework.authtoken"]
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    reveal_type(user.auth_token)  # revealed: Token
    reveal_type(user.logentry_set.get())  # revealed: LogEntry
```

### `AppConfig` classes

An entry can name an `AppConfig` class instead of a package. The `name` attribute of the class names
the package of the app, and the class can inherit it, as `AdminConfig` does from
`SimpleAdminConfig`:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
INSTALLED_APPS = [
    "django.contrib.admin.apps.AdminConfig",
    "django.contrib.auth",
    "django.contrib.contenttypes",
    "rest_framework.authtoken.apps.AuthTokenConfig",
]
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    reveal_type(user.auth_token)  # revealed: Token
    reveal_type(user.logentry_set.get())  # revealed: LogEntry
```

### `INSTALLED_APPS` that we cannot read

When a binding of `INSTALLED_APPS` is not a list of string literals that we can read, we do not know
the installed apps, and do not read the relations of any of them:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
def get_apps() -> list[str]:
    return ["rest_framework.authtoken"]

INSTALLED_APPS = ["django.contrib.admin", "django.contrib.auth", "django.contrib.contenttypes"]
INSTALLED_APPS += get_apps()
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    user.auth_token  # error: [missing-attribute]
    user.logentry_set  # error: [missing-attribute]
```

### `INSTALLED_APPS` set under a condition

A binding of `INSTALLED_APPS` that only runs under a condition counts like any other. An extra app
can only add relations, so we read the apps of every binding, even when the setting might not be
set:

`manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`mysite/__init__.py`:

```py
```

`mysite/settings.py`:

```py
import os

if os.environ.get("WITH_TOKENS") is not None:
    INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "rest_framework.authtoken"]
```

`views.py`:

```py
from django.contrib.auth.models import User

def _(user: User):
    reveal_type(user.auth_token)  # revealed: Token
```

## Field names after annotations

`annotate()` and `alias()` add names that the field names of later queryset methods can use, also
when the queryset is stored in a variable:

```py
from django.db import models
from django.db.models import Count

class Author(models.Model):
    name = models.CharField(max_length=100)

class Book(models.Model):
    title = models.CharField(max_length=200)
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

annotated = Book.objects.annotate(n=Count("author"))
annotated.order_by("-n")  # ok
annotated.order_by("nn")  # error: [invalid-argument-type]

aliased = Book.objects.alias(n=Count("author"))
aliased.order_by("n")  # ok
```

We do not see the names that `extra()` adds, so we do not check the names after it:

```py
extended = Book.objects.extra(select={"is_recent": "id > 100"})
extended.order_by("is_recent")  # ok
```

[django-stubs]: https://github.com/typeddjango/django-stubs

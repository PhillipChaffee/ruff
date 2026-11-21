# Django

Django support in ty currently relies on the stubs from [django-stubs]:

```toml
[environment]
python-version = "3.13"
python-platform = "linux"

[project]
dependencies = [
    "django-stubs @ git+https://github.com/typeddjango/django-stubs.git@44457d47e03845c27b560936ef0455d5dfc7b7d8",
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

## User model

`get_user_model()` returns the model named by the `AUTH_USER_MODEL` setting, which is
`django.contrib.auth.models.User` by default. django-stubs declares the user model as
`AbstractBaseUser`, a placeholder that a type checker is expected to refine. We do not read
settings, so we use `AbstractUser`, the base class of `User` and of most custom user models:

```py
from django.contrib.auth import get_user_model
from django.contrib.auth.models import AbstractUser
from django.http import HttpRequest

User = get_user_model()
reveal_type(User)  # revealed: <class 'AbstractUser'>
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

The result of `get_user_model()` can be used as a type:

```py
def _(user: User):
    reveal_type(user)  # revealed: AbstractUser
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

The rows keep their declared types when a field is not named by a string literal, when a lookup
spans a relation, for expressions passed as keyword arguments, and for named tuples:

```py
from django.db.models.functions import Length

def _(field: str):
    reveal_type(Article.objects.values(field).get())  # revealed: dict[str, Any]
    reveal_type(Article.objects.values_list(field).get())  # revealed: Any

reveal_type(Article.objects.values("title", n=Length("title")).get())  # revealed: dict[str, Any]
reveal_type(Article.objects.values_list("title__length").get())  # revealed: Any
reveal_type(Article.objects.values_list("title", named=True).get())  # revealed: Any
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

## Lookups

The keyword arguments of `filter()` name fields of the model. Django raises `FieldError` for an
unknown name:

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)

class Review(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE, related_name="reviews")
    rating = models.IntegerField()

class Award(models.Model):
    book = models.ForeignKey(Book, on_delete=models.CASCADE)

Book.objects.filter(titel="Example")  # error: [unknown-argument]
Book.objects.exclude(titel="Example")  # error: [unknown-argument]
Book.objects.get(titel="Example")  # error: [unknown-argument]

Book.objects.filter(title="Example", pk=1, id__in=[1, 2])  # ok
Book.objects.filter(title__startswith="Ex")  # ok
```

A lookup can also start with the name of a relation from another model, which is its `related_name`
or the lowercase name of that model:

```py
Book.objects.filter(reviews__rating=5, award__isnull=False)  # ok
Review.objects.filter(book__title="Example", book_id=1)  # ok
```

We only check the first name of a lookup, and only calls on Django's own `Manager`. A `QuerySet` can
have annotations, which are also valid names:

```py
from django.db.models.functions import Length

Book.objects.filter(title__nonexistent=1)  # ok
Book.objects.annotate(title_length=Length("title")).filter(title_length=7)  # ok
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

We do not read the project's settings module, so a setting that the project adds has the type `Any`:

```py
reveal_type(settings.PAYMENT_PROVIDER)  # revealed: Any
```

Django raises `AttributeError` for a setting that does not exist. We cannot tell a setting that does
not exist from one that the project adds:

```py
# TODO: Should be an error.
settings.NOT_A_SETTING
```

[django-stubs]: https://github.com/typeddjango/django-stubs

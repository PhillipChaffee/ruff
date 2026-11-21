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
# TODO: Reject the invalid type for `published`.
Book(
    title="The Hitchhiker's Guide to the Galaxy",
    published=123,
    pages=180,
)
```

Unrecognized keyword arguments also lead to an error:

```py
# TODO: Reject the unknown keyword `publised`.
Book(
    title="The Hitchhiker's Guide to the Galaxy",
    publised=date(1979, 10, 12),
    pages=180,
)
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

# TODO: Should not be an error, and the type should be `int`.
# error: [missing-attribute]
reveal_type(article.view_count)  # revealed: Unknown

# The original field name is not available as an attribute.
# TODO: Should be an missing-attribute error.
article.views

# This is correct:
Article(view_count=1)

# Using the original attribute name is incorrect.
# TODO: Reject the unknown keyword `views`.
Article(views=1)
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

A subclass that does not declare type parameters has no place to record the related model:

```py
class PlainForeignKey(models.ForeignKey):
    pass

class Page(models.Model):
    book = PlainForeignKey(Book, on_delete=models.CASCADE)

def _(page: Page):
    # TODO: Should be `Book`.
    reveal_type(page.book)  # revealed: Any
    # TODO: Should be `int`.
    reveal_type(page.book_id)  # revealed: Any
```

A `ForeignKey` adds a reverse accessor to the related model. By default, its name is the lowercase
source model name followed by `_set`. The `related_name` parameter overrides this name:

```py
def _(author: Author):
    # TODO: Should not be an error, and the type should be `QuerySet[Book]`.
    # error: [missing-attribute]
    reveal_type(author.book_set.all())  # revealed: Unknown

    # TODO: Should not be an error, and the type should be `QuerySet[Book]`.
    # error: [missing-attribute]
    reveal_type(author.edited_books.all())  # revealed: Unknown
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

The related model can be given as a string reference. We do not resolve the string yet, but a
foreign key declared with `null=True` is still nullable:

```py
from django.db import models

class Author(models.Model):
    mentor = models.ForeignKey("self", null=True, on_delete=models.SET_NULL)

class Book(models.Model):
    author = models.ForeignKey("Author", on_delete=models.CASCADE)
    editor = models.ForeignKey("Author", null=True, on_delete=models.SET_NULL)

def _(book: Book, author: Author):
    # TODO: Should be `Author`.
    reveal_type(book.author)  # revealed: Any
    # TODO: Should be `Author | None`.
    reveal_type(book.editor)  # revealed: Any | None
    # TODO: Should be `Author | None`.
    reveal_type(author.mentor)  # revealed: Any | None
```

The type of `<field>_id` should follow the primary key of the related model:

```py
def _(book: Book):
    # TODO: Should be `int`.
    reveal_type(book.author_id)  # revealed: Any
    # TODO: Should be `int | None`.
    reveal_type(book.editor_id)  # revealed: Any | None
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
    # TODO: Should not be an error, and the type should be `Profile`.
    # error: [missing-attribute]
    reveal_type(author.profile)  # revealed: Unknown
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
    # TODO: Should not be an error, and the type should be `QuerySet[Book]`.
    # error: [missing-attribute]
    reveal_type(author.books.all())  # revealed: Unknown
```

## User model

`get_user_model()` returns the model named by the `AUTH_USER_MODEL` setting, which is
`django.contrib.auth.models.User` by default. django-stubs declares the return type as
`type[AbstractBaseUser]`, so the result cannot be used as a type, and the attributes that `User`
adds are missing:

```py
from django.contrib.auth import get_user_model
from django.http import HttpRequest

User = get_user_model()

# TODO: Should not be an error.
# error: [invalid-type-form]
def _(user: User):
    # TODO: Should be `User`.
    reveal_type(user)  # revealed: Unknown

# TODO: Should not be an error, and the type should be `User`.
# error: [missing-attribute]
reveal_type(User.objects.create_user("alice"))  # revealed: Unknown

def _(request: HttpRequest):
    # TODO: Should be `User | AnonymousUser`.
    reveal_type(request.user)  # revealed: AbstractBaseUser | AnonymousUser

    # TODO: Should not be an error.
    # error: [missing-union-attribute]
    request.user.is_superuser
```

## Primary key of an abstract model's subclasses

An abstract model cannot be instantiated, so a value annotated with an abstract model is always an
instance of a concrete subclass, which has a primary key:

```py
from django.db import models

class Publication(models.Model):
    class Meta:
        abstract = True

class Book(Publication):
    pass

def _(publication: Publication):
    # TODO: Should follow the primary key of the concrete subclasses.
    reveal_type(publication.pk)  # revealed: Any

    # TODO: Should not be an error when every concrete subclass has an `id` field.
    publication.id  # error: [missing-attribute]
```

## Choices classes

The `choices` parameter accepts a subclass of Django's `Choices`, such as `TextChoices`:

```py
from django.db import models

class Status(models.TextChoices):
    DRAFT = "D", "Draft"
    PUBLISHED = "P", "Published"

class Article(models.Model):
    # TODO: Should not be an error.
    # error: [invalid-argument-type]
    status = models.CharField(max_length=1, choices=Status, default=Status.DRAFT)

def _(article: Article):
    reveal_type(article.status)  # revealed: str
    reveal_type(article.get_status_display())  # revealed: str
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

# TODO: Should not be an error, and the type should be `ArticleQuerySet`. django-stubs declares
# the return type of `from_queryset` as `type[Self]`, which loses the `QuerySet` class.
# error: [missing-attribute]
reveal_type(Article.reviewed.published())  # revealed: Unknown
```

The manager only has the public methods that the custom `QuerySet` class adds. Django does not copy
private methods, other attributes, or the methods of `QuerySet` that only make sense on a `QuerySet`,
such as `delete()`:

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
single value with `flat=True`. The types should follow the named fields:

```py
from django.db import models

class Article(models.Model):
    title = models.CharField(max_length=100)
    rating = models.IntegerField()

# TODO: Should be a `TypedDict` with a `title` key of type `str`.
reveal_type(Article.objects.values("title").get())  # revealed: dict[str, Any]

# TODO: Should be `tuple[str, int]`.
reveal_type(Article.objects.values_list("title", "rating").get())  # revealed: Any

# TODO: Should be `str`.
reveal_type(Article.objects.values_list("title", flat=True).get())  # revealed: Any
```

## Annotations

`annotate()` adds an attribute to each instance in the `QuerySet` for every keyword argument:

```py
from django.db import models
from django.db.models.functions import Length

class Article(models.Model):
    title = models.CharField(max_length=100)

article = Article.objects.annotate(title_length=Length("title")).get()

# TODO: Should not be an error, and the type should be `int`.
# error: [missing-attribute]
reveal_type(article.title_length)  # revealed: Unknown
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

The automatic primary key accepts a string for the same reason. Django also allows resetting it to
`None`, which makes the next `save()` insert a copy:

```py
def _(book: Book):
    book.id = "1"  # ok

    # TODO: Should not be an error.
    book.pk = None  # error: [invalid-assignment]
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
    reveal_type(book.tags)  # revealed: ManyRelatedManager[Tag, Any]
    reveal_type(book.tags.all())  # revealed: QuerySet[Tag, Tag]

# TODO: Should be the automatically created `Book_tags` model.
reveal_type(Book.tags.through)  # revealed: type[Any]
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

# TODO: Should be an error.
Book.objects.filter(titel="Example")
```

## Model exceptions

Every model has its own `DoesNotExist` exception, a subclass of `ObjectDoesNotExist`:

```py
from django.db import models

class Book(models.Model):
    pass

# TODO: Should be the `DoesNotExist` class of `Book`.
reveal_type(Book.DoesNotExist)  # revealed: type[ObjectDoesNotExist]
```

## Multi-table inheritance

A model that inherits from a concrete model has a `<parent>_ptr` field that links to the parent
model, and the parent has a reverse accessor named after the child:

```py
from django.db import models

class Book(models.Model):
    pass

class Ebook(Book):
    pass

def _(book: Book, ebook: Ebook):
    # TODO: Should not be an error, and the type should be `Book`.
    # error: [missing-attribute]
    reveal_type(ebook.book_ptr)  # revealed: Unknown

    # TODO: Should not be an error, and the type should be `int`.
    # error: [missing-attribute]
    reveal_type(ebook.book_ptr_id)  # revealed: Unknown

    # TODO: Should not be an error, and the type should be `Ebook`.
    # error: [missing-attribute]
    reveal_type(book.ebook)  # revealed: Unknown
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

`django.conf.settings` gives access to the settings of the project. Django raises `AttributeError`
for a setting that does not exist:

```py
from django.conf import settings

# TODO: Should be `bool`.
reveal_type(settings.DEBUG)  # revealed: Any

# TODO: Should be an error.
settings.NOT_A_SETTING
```

[django-stubs]: https://github.com/typeddjango/django-stubs

# Django code on an extra search path

A project can keep its Django code in a directory that it adds as an extra search path, as mypy's
`mypy_path` does. The modules there are the project's code: we read its settings, find its `models`
modules, and check the field names of its models.

```toml
[environment]
python-version = "3.13"
python-platform = "linux"
extra-paths = ["/src/server"]

[project]
dependencies = [
    "django-stubs @ git+https://github.com/typeddjango/django-stubs.git@44457d47e03845c27b560936ef0455d5dfc7b7d8",
]
```

## Settings, relations and field names

`server/manage.py`:

```py
import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
```

`server/mysite/__init__.py`:

```py
```

`server/mysite/settings.py`:

```py
INSTALLED_APPS = ["django.contrib.auth", "django.contrib.contenttypes", "accounts", "library"]
AUTH_USER_MODEL = "accounts.Member"
```

`server/accounts/__init__.py`:

```py
```

`server/accounts/models.py`:

```py
from django.contrib.auth.models import AbstractUser
from django.db import models

class Member(AbstractUser):
    nickname = models.CharField(max_length=50)
```

`server/library/__init__.py`:

```py
```

`server/library/models.py`:

```py
from django.db import models

class Book(models.Model):
    title = models.CharField(max_length=200)
    reader = models.ForeignKey("accounts.Member", on_delete=models.CASCADE, related_name="books")
```

`server/library/views.py`:

```py
from django.contrib.auth import get_user_model

from accounts.models import Member
from library.models import Book

reveal_type(get_user_model())  # revealed: type[Member]

def _(member: Member):
    reveal_type(member.books)  # revealed: RelatedManager[Book, QuerySet[Book, Book]]

Book.objects.order_by("titel")  # error: [invalid-argument-type] "Cannot resolve keyword 'titel' into field"
```

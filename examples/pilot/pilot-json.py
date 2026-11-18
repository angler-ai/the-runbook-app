#!/usr/bin/env python3
"""Small JSON helper for the pilot install guide (standard library only).

Reads one JSON document on stdin.

  pilot-json.py get PATH
      Print the value at PATH (dots between keys, numbers index lists;
      `.` is the whole document). Strings print raw. Exit 1 if missing.

  pilot-json.py find PATH KEY=VALUE FIELD
      In the list at PATH, print FIELD of the first item whose KEY equals
      VALUE (compared as text). Exit 1 if no item matches.

  pilot-json.py items FILE [FILE ...]
      Join the `items` of several list pages (files; stdin is not read) into
      one {"items": [...]} document.

  pilot-json.py keep DIR FIELD=FILE [FIELD=FILE ...]
      Store each secret FIELD in DIR/FILE, created with mode 0600 and never
      overwritten, then print the document with those fields replaced by
      "[stored in FILE]". Nothing is stored unless every field is present.
      Use it on `--json` output that carries a secret (`token create`,
      `webhook create`, `runbook-server bootstrap`, `POST /v1/tokens`).

Secrets are never printed. Run it as `python3 -I pilot-json.py ...`.
"""
import json
import os
import re
import sys

SAFE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def fail(message):
    sys.stderr.write("pilot-json: %s\n" % message)
    sys.exit(1)


def lookup(document, path):
    value = document
    if path in ("", "."):
        return value
    for part in path.split("."):
        if isinstance(value, list) and part.isdigit() and int(part) < len(value):
            value = value[int(part)]
        elif isinstance(value, dict) and part in value:
            value = value[part]
        else:
            raise KeyError(path)
    return value


def show(value):
    if isinstance(value, str):
        print(value)
    else:
        print(json.dumps(value))


def replace(document, path, text):
    parts = path.split(".")
    parent = lookup(document, ".".join(parts[:-1])) if len(parts) > 1 else document
    key = parts[-1]
    if isinstance(parent, list):
        parent[int(key)] = text
    else:
        parent[key] = text


def keep(document, directory, pairs):
    if not os.path.isdir(directory):
        fail("directory %r does not exist" % directory)
    plan = []
    for pair in pairs:
        field, sep, name = pair.partition("=")
        if not sep or not field or not SAFE_NAME.match(name):
            fail("expected FIELD=FILE with a plain file name, got %r" % pair)
        try:
            value = lookup(document, field)
        except KeyError:
            fail("the input has no %r field; nothing was stored" % field)
        if not isinstance(value, str) or not value:
            fail("field %r is not a non-empty string; nothing was stored" % field)
        target = os.path.join(directory, name)
        if os.path.lexists(target):
            fail("%s already exists; refusing to overwrite a stored secret" % target)
        plan.append((field, name, target, value))
    for field, name, target, value in plan:
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(value + "\n")
        replace(document, field, "[stored in %s]" % name)
    print(json.dumps(document, indent=2, sort_keys=True))


def main(argv):
    if len(argv) < 2 or argv[0] not in ("get", "find", "items", "keep"):
        fail("usage: pilot-json.py get PATH | find PATH KEY=VALUE FIELD | items FILE... | keep DIR FIELD=FILE...")
    if argv[0] == "items":
        joined = []
        for path in argv[1:]:
            try:
                with open(path, encoding="utf-8") as f:
                    page = json.load(f)
            except (OSError, ValueError):
                fail("%s is not a JSON list page" % path)
            items = page.get("items") if isinstance(page, dict) else None
            if not isinstance(items, list):
                fail("%s has no items list" % path)
            joined.extend(items)
        print(json.dumps({"items": joined}))
        return
    try:
        document = json.load(sys.stdin)
    except ValueError:
        fail("stdin is not JSON")
    mode = argv[0]
    if mode == "get" and len(argv) == 2:
        try:
            show(lookup(document, argv[1]))
        except KeyError:
            fail("no %r in the input" % argv[1])
    elif mode == "find" and len(argv) == 4:
        key, sep, wanted = argv[2].partition("=")
        if not sep:
            fail("expected KEY=VALUE, got %r" % argv[2])
        try:
            items = lookup(document, argv[1])
        except KeyError:
            fail("no %r in the input" % argv[1])
        for item in items if isinstance(items, list) else []:
            if not isinstance(item, dict):
                continue
            current = item.get(key)
            text = current if isinstance(current, str) else json.dumps(current)
            if text == wanted and argv[3] in item:
                show(item[argv[3]])
                return
        sys.exit(1)
    elif mode == "keep" and len(argv) >= 3:
        keep(document, argv[1], argv[2:])
    else:
        fail("wrong number of arguments for %s" % mode)


if __name__ == "__main__":
    main(sys.argv[1:])

"""Shared host wire for the Files module's components. One send() so the
dispatcher, browser, and editor all speak on the same stdout stream."""
import json
import sys


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})


# Alt+Backspace arrives as ESC + DEL ("\x1b\x7f") from the host across the
# whole app. This is the single source of truth for that byte sequence.
WORD_BACKSPACE = "\x1b\x7f"


def del_word_back(s):
    """Drop the trailing word of `s` (plus the run of spaces before it) —
    the text-input flavor of Alt+Backspace for short fields (filter,
    name entry, find, save-as). The editor's body uses its own
    punctuation-aware word boundary."""
    i = len(s)
    while i > 0 and s[i - 1].isspace():
        i -= 1
    while i > 0 and not s[i - 1].isspace():
        i -= 1
    return s[:i]

"""Shared host wire for the Files module's components. One send() so the
dispatcher, browser, and editor all speak on the same stdout stream."""
import json
import sys


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def log(message):
    send({"kind": "log", "message": message})

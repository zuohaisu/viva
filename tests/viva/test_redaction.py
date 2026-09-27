"""Secret redaction: one implementation, used by every Viva artifact write."""

from __future__ import annotations

from viva.core.redaction import MASK, redact, redact_file


def test_secret_shaped_keys_are_masked_wholesale():
    cleaned = redact({"api_key": "abc", "nested": {"password": "p"}, "ok": "keep", "empty_token": ""})
    assert cleaned["api_key"] == MASK
    assert cleaned["nested"]["password"] == MASK
    assert cleaned["ok"] == "keep"
    assert cleaned["empty_token"] == ""


def test_token_patterns_are_masked_inside_free_text():
    text = "use ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345 and sk-abcdefgh12345678 now"
    cleaned = redact(text)
    assert "ghp_" not in cleaned
    assert "sk-" not in cleaned
    assert cleaned.count(MASK) == 2


def test_known_secrets_are_removed_wherever_they_appear():
    cleaned = redact({"message": "token is hunter2 here"}, known_secrets=["hunter2"])
    assert "hunter2" not in cleaned["message"]


def test_lists_and_tuples_are_walked():
    cleaned = redact({"items": [{"secret": "x"}, "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"], "t": ("a",)})
    assert cleaned["items"][0]["secret"] == MASK
    assert MASK in cleaned["items"][1]
    assert cleaned["t"] == ["a"]


def test_redact_file_rewrites_only_when_needed(tmp_path):
    path = tmp_path / "log.txt"
    path.write_text("hello world\n", encoding="utf-8")
    assert redact_file(path) is False
    assert path.read_text(encoding="utf-8") == "hello world\n"

    path.write_text("leaked ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\n", encoding="utf-8")
    assert redact_file(path) is True
    assert MASK in path.read_text(encoding="utf-8")

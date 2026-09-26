"""Resident registry: generic identity records, persistence, no seeded persona."""

from __future__ import annotations

import pytest

from viva.residents import ResidentError, ResidentRegistry


def test_fresh_home_has_no_residents(home):
    registry = ResidentRegistry(home)
    assert registry.list() == []


def test_create_and_reload_persists(home):
    registry = ResidentRegistry(home)
    record = registry.create("Test Resident", notes="first")
    assert record["id"] == "test-resident"
    assert record["name"] == "Test Resident"
    assert record["created_at"].endswith("Z")
    assert record["schema_version"] == "1.0"

    reloaded = ResidentRegistry(home)
    assert [item["id"] for item in reloaded.list()] == ["test-resident"]
    found = reloaded.get("Test RESIDENT")  # case-insensitive name lookup
    assert found is not None and found["notes"] == "first"


def test_get_by_exact_id(home):
    registry = ResidentRegistry(home)
    registry.create("Alice")
    assert registry.get("alice")["name"] == "Alice"


def test_duplicate_resident_rejected(home):
    registry = ResidentRegistry(home)
    registry.create("Maya")
    with pytest.raises(ResidentError, match="already exists"):
        registry.create("Maya")


def test_invalid_names_rejected(home):
    registry = ResidentRegistry(home)
    with pytest.raises(ResidentError):
        registry.create("   ")
    with pytest.raises(ResidentError):
        registry.create("!!!")
    with pytest.raises(ResidentError):
        registry.create("Ok", resident_id="not a slug!")


def test_registry_is_generic_no_core_persona(home):
    """Core must not ship any default resident — Samuel is user data, not code."""
    registry = ResidentRegistry(home)
    assert registry.get("samuel") is None
    registry.create("Samuel")  # as user data it works like any other name
    import pathlib

    import viva

    package_root = pathlib.Path(viva.__file__).parent
    for path in package_root.rglob("*.py"):
        assert "Samuel" not in path.read_text(encoding="utf-8"), (
            f"product code hard-codes a persona: {path}"
        )

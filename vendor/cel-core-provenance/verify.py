#!/usr/bin/env python3
"""Verify the maintained cel-core fork against its pinned upstream archive."""

import argparse
import hashlib
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
VENDOR = HERE.parent / "cel-core"
EXPECTED_CRATE = "6b317c80c3b45ab6c8a636feaf0e51798a2c7394cee9a1f54ed83d2addb1ecb5"
LICENSES = {
    "LICENSE-MIT": "d611016d5f19df41427f68511735672d14319aceb85d9119eb9d469ac1254e60",
    "LICENSE-APACHE": "87f4be1ace1f3fe7b3bf000ed0a80c7d54d8a0e8a29f07ab0fb4310ed1bcc2bf",
}


class VerificationError(Exception):
    """An unavailable input or a mismatch in the recorded provenance."""


def find_archive():
    cargo_home = pathlib.Path(os.environ.get("CARGO_HOME", pathlib.Path.home() / ".cargo"))
    archives = sorted((cargo_home / "registry/cache").glob("*/cel-core-0.5.1.crate"))
    if not archives:
        raise VerificationError(
            "cel-core-0.5.1.crate not cached; fetch the pinned crate first "
            "or supply --archive (see PROVENANCE.md)"
        )
    return archives[0]


def reconstruct(archive, destination):
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if digest != EXPECTED_CRATE:
        raise VerificationError(f"upstream archive SHA-256 mismatch: {archive}")
    with tarfile.open(archive) as crate:
        crate.extractall(destination, filter="data")
    upstream = pathlib.Path(destination) / "cel-core-0.5.1"
    result = subprocess.run([
        "patch", "--batch", "--fuzz=0", "-p1", "-i",
        str(HERE / "cel-core-0.5.1.patch"),
    ], cwd=upstream, text=True, capture_output=True)
    if result.returncode:
        raise VerificationError("recorded patch cannot be applied:\n" + result.stdout + result.stderr)
    return upstream


def inventory(tree):
    if not tree.is_dir():
        raise VerificationError(f"missing vendor directory: {tree}")
    files = set()
    for path in tree.rglob("*"):
        relative = path.relative_to(tree)
        # Cargo output is not source; all packaged files, including Cargo.lock, are checked.
        if relative.parts[0] == "target":
            continue
        if path.is_symlink():
            raise VerificationError(f"unexpected symlink: {relative.as_posix()}")
        if path.is_file():
            files.add(relative)
        elif not path.is_dir():
            raise VerificationError(f"unexpected non-regular file: {relative.as_posix()}")
    return files


def verify_tree(tree, archive):
    with tempfile.TemporaryDirectory(prefix="runbook-cel-upstream-") as temp:
        upstream = reconstruct(archive, temp)
        expected = inventory(upstream)
        actual = inventory(tree)
        expected_with_licenses = expected | {pathlib.Path(name) for name in LICENSES}
        missing = sorted(expected_with_licenses - actual)
        extra = sorted(actual - expected_with_licenses)
        if missing:
            raise VerificationError("missing file: " + missing[0].as_posix())
        if extra:
            raise VerificationError("unrecorded file: " + extra[0].as_posix())
        for relative in sorted(expected):
            if (upstream / relative).read_bytes() != (tree / relative).read_bytes():
                raise VerificationError("content mismatch: " + relative.as_posix())
    for name, expected_digest in LICENSES.items():
        if hashlib.sha256((tree / name).read_bytes()).hexdigest() != expected_digest:
            raise VerificationError("license SHA-256 mismatch: " + name)


class VerifierTests(unittest.TestCase):
    """A verifier that skips content comparison must fail these real CLI tests."""

    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="runbook-cel-verifier-test-")
        cls.addClassCleanup(cls.temp.cleanup)
        archive = cls.archive
        assert hashlib.sha256(archive.read_bytes()).hexdigest() == EXPECTED_CRATE
        with tarfile.open(archive) as crate:
            crate.extractall(cls.temp.name, filter="data")
        cls.original = pathlib.Path(cls.temp.name) / "cel-core-0.5.1"
        subprocess.run([
            "patch", "--batch", "--fuzz=0", "-p1", "-i",
            str(HERE / "cel-core-0.5.1.patch"),
        ], cwd=cls.original, check=True, capture_output=True)
        for name, digest in LICENSES.items():
            data = (VENDOR / name).read_bytes()
            assert hashlib.sha256(data).hexdigest() == digest, name
            (cls.original / name).write_bytes(data)

    def run_verifier(self, tree):
        return subprocess.run(
            [sys.executable, str(HERE / "verify.py"), "--tree", str(tree),
             "--archive", str(self.archive)],
            text=True, capture_output=True,
        )

    def test_original_tree_passes(self):
        for tree in (self.original, VENDOR):
            with self.subTest(tree=str(tree)):
                result = self.run_verifier(tree)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_tampered_source_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="runbook-cel-tampered-") as temp:
            tree = pathlib.Path(temp) / "cel-core"
            shutil.copytree(self.original, tree)
            relative = "src/lib.rs"
            source = tree / relative
            data = source.read_bytes()
            source.write_bytes(bytes([data[0] ^ 1]) + data[1:])
            result = self.run_verifier(tree)
            self.assertNotEqual(result.returncode, 0, "one-byte tamper was accepted")
            self.assertIn(relative, result.stdout + result.stderr)

    def test_manifest_examples_vcs_and_licenses_are_checked(self):
        for relative in ("Cargo.toml", "Cargo.toml.orig", "Cargo.lock",
                         "examples/hello.rs", ".cargo_vcs_info.json", *LICENSES):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as temp:
                tree = pathlib.Path(temp) / "cel-core"
                shutil.copytree(self.original, tree)
                with (tree / relative).open("ab") as output:
                    output.write(b"\nTAMPERED\n")
                result = self.run_verifier(tree)
                self.assertNotEqual(result.returncode, 0, relative)
                self.assertIn(relative, result.stdout + result.stderr)

    def test_unrecorded_files_and_missing_source_are_rejected(self):
        for relative in ("src/unrecorded.txt", "examples/unrecorded.rs", "src/lib.rs"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as temp:
                tree = pathlib.Path(temp) / "cel-core"
                shutil.copytree(self.original, tree)
                path = tree / relative
                if path.exists():
                    path.unlink()
                else:
                    path.write_text("unrecorded\n")
                result = self.run_verifier(tree)
                self.assertNotEqual(result.returncode, 0, relative)
                self.assertIn(relative, result.stdout + result.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="exercise real verifier tamper rejection")
    parser.add_argument("--tree", type=pathlib.Path, default=VENDOR, help="tree to verify (default: maintained vendor)")
    parser.add_argument("--archive", type=pathlib.Path, help="pinned upstream .crate (default: Cargo cache)")
    args = parser.parse_args()
    try:
        archive = args.archive if args.archive is not None else find_archive()
        if not args.self_test:
            verify_tree(args.tree, archive)
            print("PASS: pinned upstream + exact patch reproduces all packaged files; license hashes match")
            return 0
    except (VerificationError, OSError, tarfile.TarError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    if args.self_test:
        VerifierTests.archive = archive
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(VerifierTests)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())

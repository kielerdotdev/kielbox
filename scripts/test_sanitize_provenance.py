import importlib.util
import json
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("sanitize-provenance.py")
spec = importlib.util.spec_from_file_location("sanitize_provenance", MODULE_PATH)
sanitizer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sanitizer)


class SanitizeProvenanceTests(unittest.TestCase):
    def test_paths_ids_and_public_values_are_preserved_recursively(self):
        document = {
            "inputs": {
                "root": r"C:\Users\SyntheticUser\Documents\demo",
                "source_reference": "https://example.invalid/repo/commit/abc123",
                "archive_sha256": "a" * 64,
                "runtime_directory": r"C:\bin",
            },
            "generation": {
                "actual_included_headers": [
                    {"path": r"C:\Program Files (x86)\Windows Kits\10\Include\10.0.1\um\windows.h"},
                    {"path": r"C:\Program Files\Microsoft Visual Studio\VC\Tools\MSVC\14.1\include\stddef.h"},
                    {"path": r"C:\Users\SyntheticUser\Documents\demo\.downloads\tooling\ffmpeg7-bindings\inputs-0123456789abcdef0123456789abcdef\include\libavcodec\avcodec.h"},
                ],
                "clang_args": [r"-IC:\Program Files\Microsoft Visual Studio\VC\Tools\MSVC\14.1\include"],
                "runtime_versions": {"avcodec.dll": {"path": r"C:\bin\avcodec.dll"}},
            },
            "configuration": {
                "headers": [r"C:\Users\SyntheticUser\Documents\demo\src\wrapper.h"],
                "sdk": r"C:/Program Files (x86)/Windows Kits/10/Include/10.0.1/shared/windows.h",
            },
            "profile_posix": "/home/SyntheticUser/.cache/tool",
            "other_posix": "/Users/SyntheticUser/Library/Application Support/example",
            "unknown_local": r"D:\temp\private.bin",
            "input_id": "inputs-0123456789abcdef0123456789abcdef",
        }
        result = sanitizer.sanitize_document(document)
        encoded = json.dumps(result)

        self.assertEqual(result["inputs"]["root"], "<workspace>")
        self.assertEqual(result["inputs"]["source_reference"], document["inputs"]["source_reference"])
        self.assertEqual(result["inputs"]["archive_sha256"], document["inputs"]["archive_sha256"])
        headers = result["generation"]["actual_included_headers"]
        self.assertEqual(headers[0]["path"], "<windows-sdk-include>/10.0.1/um/windows.h")
        self.assertEqual(headers[1]["path"], "<msvc-include>/stddef.h")
        self.assertEqual(headers[2]["path"], "<tooling>/ffmpeg7-bindings/inputs-<id>/include/libavcodec/avcodec.h")
        self.assertEqual(result["generation"]["runtime_versions"]["avcodec.dll"]["path"], "<local-path>/avcodec.dll")
        self.assertEqual(result["inputs"]["runtime_directory"], "<ffmpeg-runtime>")
        self.assertEqual(result["profile_posix"], "<user-home>/.cache/tool")
        self.assertEqual(result["other_posix"], "<user-home>/Library/Application Support/example")
        self.assertEqual(result["unknown_local"], "<local-path>/private.bin")
        self.assertEqual(result["configuration"]["headers"][0], "<workspace>/src/wrapper.h")
        self.assertEqual(result["configuration"]["sdk"], "<windows-sdk-include>/10.0.1/shared/windows.h")
        self.assertEqual(result["generation"]["clang_args"][0], "-I<msvc-include>")
        self.assertIn("inputs-<id>", encoded)
        self.assertNotIn("SyntheticUser", encoded)
        self.assertNotIn("C:\\Users\\", encoded)
        self.assertNotIn("D:\\", encoded)

    def test_non_string_values_and_public_values_survive(self):
        source = {"url": "https://example.invalid/archive.zip", "count": 4, "enabled": False}
        self.assertEqual(sanitizer.sanitize_document(source), source)


if __name__ == "__main__":
    unittest.main()

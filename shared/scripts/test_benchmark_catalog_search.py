#!/usr/bin/env python3
"""Isolated production-harness tests; never start an engine binary."""
from contextlib import closing, redirect_stderr, redirect_stdout
from copy import deepcopy
import hashlib
import io
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import benchmark_catalog_search as benchmark


class Clock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now


VALID_RESPONSE = {
    "requestID": "fixture-request", "status": "ok", "chapters": [], "jobs": [],
    "hits": [{"fileID": 42, "path": "/internal/Family Library/Bucket42 - File 42.mov",
              "kind": "video", "text": "Alex plays baseball and opens birthday presents"}],
}


class FakeEngine:
    instances = []
    clock = None
    fail_ready = None
    fail_request = None
    response = VALID_RESPONSE
    mutate_binary = False

    def __init__(self, binary, directory, swift):
        self.binary = binary
        self.directory = directory
        self.swift = swift
        self.closed = False
        self.calls = []
        self.seen_queries = set()
        self.first_use_queries = []
        self.ordinal = len(self.instances)
        self.instances.append(self)
        self.clock.now += 0.002
        database = directory / "FileID/fileid.sqlite"
        database.parent.mkdir(exist_ok=True)
        with closing(sqlite3.connect(database)) as db, db:
            db.execute("CREATE TABLE IF NOT EXISTS files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,vlm_description)")
            db.execute("CREATE TABLE IF NOT EXISTS catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited)")
            db.execute("CREATE TABLE IF NOT EXISTS catalog_passages(id,file_id,page,text,source_revision,model_version,confidence)")

    def wait(self, name):
        assert name == "ready"
        self.clock.now += 0.003
        if self.ordinal == self.fail_ready:
            raise RuntimeError("fake ready failure")

    def request(self, action, **fields):
        assert action == "search"
        self.calls.append(fields["query"])
        index = len(self.calls)
        if (self.ordinal, index) == self.fail_request:
            raise RuntimeError("fake request failure")
        if self.mutate_binary:
            self.binary.write_bytes(b"changed fake binary")
        query = fields["query"]
        query_index = benchmark.QUERIES.index(query)
        penalty_ms = 0
        if query not in self.seen_queries:
            self.seen_queries.add(query)
            self.first_use_queries.append(query)
            penalty_ms = 1000
        duration_ms = self.ordinal * 100 + query_index * 10 + index / 10 + penalty_ms
        self.clock.now += duration_ms / 1000
        return self.response

    def close(self):
        assert self.directory.is_dir(), "fixture deleted before engine stop"
        self.closed = True


class BenchmarkCatalogSearchTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(tempfile.gettempdir()) / "FileIDHarnessUnitTests"
        scratch.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="test-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / "fake-engine"
        self.binary.write_bytes(b"not an executable; fake Engine only")
        self.clock = Clock()
        FakeEngine.instances = []
        FakeEngine.clock = self.clock
        FakeEngine.fail_ready = None
        FakeEngine.fail_request = None
        FakeEngine.response = deepcopy(VALID_RESPONSE)
        FakeEngine.mutate_binary = False
        for target, value in (("Engine", FakeEngine), ("time.perf_counter", self.clock),
                              ("tempfile.gettempdir", lambda: str(self.root))):
            patcher = patch("benchmark_catalog_search." + target, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def argv(self, *extra):
        return ["--engine", str(self.binary), "--runtime", "rust", "--files", "100", "--requests", "10", *extra]

    def run_main(self, *extra):
        output = io.StringIO()
        with redirect_stdout(output):
            benchmark.main(self.argv(*extra))
        return json.loads(output.getvalue())

    def assert_stopped_and_removed(self):
        self.assertTrue(FakeEngine.instances)
        self.assertTrue(all(engine.closed for engine in FakeEngine.instances))
        self.assertTrue(all(not engine.directory.exists() for engine in FakeEngine.instances))

    def test_legacy_cli_defaults_hashes_and_scope(self):
        result = self.run_main()
        self.assertEqual((result["runtime"], result["files"], result["requests"]), ("rust", 100, 10))
        self.assertEqual((result["trial_count"], result["warmups"], result["sample_count"]), (3, 10, 30))
        self.assertEqual(len(FakeEngine.instances), 4)
        self.assertEqual(result["engine"]["sha256"], hashlib.sha256(self.binary.read_bytes()).hexdigest())
        self.assertEqual(result["engine"]["build_profile"], "unknown")
        self.assertEqual(result["harness"]["sha256"], benchmark.sha256_file(Path(benchmark.__file__)))
        self.assertEqual(result["fixture"], benchmark.fixture_manifest(100))
        self.assertIn("lexical", result["scope"])
        self.assertIn("warm/unknown", result["conditions"]["filesystem_cache"])
        self.assertNotIn("node", result["host"])
        self.assertEqual(result["median_ms"], 222.05)
        self.assertEqual(result["p95_ms"], 332.4)
        self.assertEqual(result["maximum_ms"], 342.5)
        self.assertEqual(result["priming_queries_per_trial"], 5)
        self.assertEqual(result["query_count"], 75)
        self.assert_stopped_and_removed()

    def test_low_warmups_exclude_all_first_uses_and_pool_distinct_trials(self):
        result = json.loads(benchmark.benchmark(benchmark.parse_args(
            self.argv("--trials", "2", "--warmups", "2", "--requests", "11"))))
        expected_samples = (
            [100.8, 110.9, 121.0, 131.1, 141.2, 101.3, 111.4, 121.5, 131.6, 141.7, 101.8],
            [200.8, 210.9, 221.0, 231.1, 241.2, 201.3, 211.4, 221.5, 231.6, 241.7, 201.8],
        )
        expected_query_summaries = (
            [(101.3, 101.3, 101.8), (111.15, 110.9, 111.4), (121.25, 121.0, 121.5),
             (131.35, 131.1, 131.6), (141.45, 141.2, 141.7)],
            [(201.3, 201.3, 201.8), (211.15, 210.9, 211.4), (221.25, 221.0, 221.5),
             (231.35, 231.1, 231.6), (241.45, 241.2, 241.7)],
        )
        for index, trial in enumerate(result["trials"]):
            self.assertAlmostEqual(trial["process_ready_ms"], 5.0)
            self.assertAlmostEqual(trial["first_query"]["elapsed_ms"], 1100.1 + index * 100)
            self.assertEqual((trial["priming_count"], trial["warmup_count"], trial["query_count"]), (5, 2, 18))
            self.assertEqual([s["query"] for s in trial["remaining_priming_samples"]], list(benchmark.QUERIES[1:]))
            for sample in trial["remaining_priming_samples"]:
                self.assertGreater(sample["elapsed_ms"], 1000)
            self.assertEqual(trial["sample_count"], 11)
            self.assertEqual((trial["median_ms"], trial["p95_ms"], trial["maximum_ms"]),
                             (121.0 + index * 100, 141.2 + index * 100, 141.7 + index * 100))
            self.assertEqual([s["query"] for s in trial["samples"]],
                             [benchmark.QUERIES[i % 5] for i in range(11)])
            for sample, expected in zip(trial["samples"], expected_samples[index]):
                self.assertAlmostEqual(sample["elapsed_ms"], expected)
            self.assertEqual([q["sample_count"] for q in trial["per_query"]], [3, 2, 2, 2, 2])
            for query, expected in zip(trial["per_query"], expected_query_summaries[index]):
                self.assertEqual((query["median_ms"], query["p95_ms"], query["maximum_ms"]), expected)
            engine = FakeEngine.instances[index + 1]
            self.assertEqual(engine.calls[:5], list(benchmark.QUERIES))
            self.assertEqual(engine.first_use_queries, list(benchmark.QUERIES))
            self.assertEqual(len(engine.calls), 18)
        self.assertEqual(result["query_count"], 36)
        self.assertEqual((result["median_ms"], result["p95_ms"], result["maximum_ms"]), (171.25, 231.6, 241.7))
        self.assert_stopped_and_removed()

    def test_swift_zero_warmups_caller_metadata_and_optional_output(self):
        output = self.root / "result.json"
        result = self.run_main("--runtime", "swift", "--trials", "1", "--warmups", "0",
                               "--build-profile", "release", "--source-revision", "fixture-rev",
                               "--conditions", "isolated fake test", "--output", str(output))
        self.assertEqual(json.loads(output.read_text()), result)
        self.assertTrue(all(engine.swift for engine in FakeEngine.instances))
        self.assertEqual(result["engine"]["build_profile"], "release")
        self.assertEqual(result["conditions"]["caller_supplied"], "isolated fake test")
        self.assertEqual(result["engine"]["source_revision"], "fixture-rev")
        trial = result["trials"][0]
        self.assertEqual(trial["median_ms"], 121.05)
        self.assertEqual((trial["priming_count"], trial["warmup_count"], trial["query_count"]), (5, 0, 15))
        self.assertEqual(FakeEngine.instances[1].calls[:5], list(benchmark.QUERIES))
        self.assertEqual(FakeEngine.instances[1].first_use_queries, list(benchmark.QUERIES))
        expected_samples = [100.6, 110.7, 120.8, 130.9, 141.0, 101.1, 111.2, 121.3, 131.4, 141.5]
        for sample, expected in zip(trial["samples"], expected_samples):
            self.assertAlmostEqual(sample["elapsed_ms"], expected)
        self.assertEqual(result["query_count"], 15)
        self.assert_stopped_and_removed()

    def test_manifest_determinism_and_seed_rows(self):
        first = benchmark.fixture_manifest(100)
        self.assertEqual(first, benchmark.fixture_manifest(100))
        self.assertNotEqual(first["rows_sha256"], benchmark.fixture_manifest(101)["rows_sha256"])
        self.assertEqual(first["row_counts"], {"files": 100, "catalog_chapters": 1, "catalog_passages": 1})
        engine = FakeEngine(self.binary, self.root, False)
        engine.close()
        benchmark.seed_fixture(self.root, 100)
        database = self.root / "FileID/fileid.sqlite"
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM files").fetchone()[0], 100)
            self.assertEqual(db.execute("SELECT path_text FROM files WHERE id=10").fetchone()[0],
                             "/internal/Family Library/Bucket10 - File 10.pdf")
        with self.assertRaisesRegex(RuntimeError, "nonempty"):
            benchmark.seed_fixture(self.root, 100)
        database.rename(database.with_suffix(".closed"))

    def test_argument_bounds_and_no_engine_execution(self):
        invalid = [("--files", "99"), ("--files", "1000001"), ("--requests", "9"),
                   ("--requests", "10001"), ("--trials", "0"), ("--trials", "21"),
                   ("--warmups", "-1"), ("--warmups", "1001"),
                   ("--requests", "10000", "--trials", "20"), ("--files", "bad"),
                   ("--build-profile", ""), ("--conditions", "x" * 1025),
                   ("--engine", str(self.root / "missing")),
                   ("--output", str(self.root / "missing-parent/result.json"))]
        for options in invalid:
            with self.subTest(options=options), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                benchmark.main(self.argv(*options))
        self.assertEqual(FakeEngine.instances, [])
        args = benchmark.parse_args(self.argv("--files", "1000000", "--requests", "10000",
                                              "--trials", "1", "--warmups", "1000"))
        self.assertEqual(args.files, 1000000)

    def test_total_query_bound_includes_all_priming_queries(self):
        args = benchmark.parse_args(self.argv("--trials", "20", "--warmups", "0", "--requests", "4995"))
        self.assertEqual(args.trials * (5 + args.warmups + args.requests), 100000)
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            benchmark.parse_args(self.argv("--trials", "20", "--warmups", "0", "--requests", "4996"))
        self.assertEqual(FakeEngine.instances, [])

    def test_output_reuse_and_race_fail_closed(self):
        output = self.root / "existing.json"
        output.write_text("original")
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            benchmark.main(self.argv("--output", str(output)))
        self.assertEqual(FakeEngine.instances, [])
        self.assertEqual(output.read_text(), "original")
        args = benchmark.parse_args(self.argv("--output", str(self.root / "race.json")))
        args.output.write_text("racing writer")
        with patch.object(benchmark, "parse_args", return_value=args), redirect_stdout(io.StringIO()), self.assertRaises(FileExistsError):
            benchmark.main([])
        self.assertEqual(args.output.read_text(), "racing writer")
        self.assert_stopped_and_removed()

    def test_ready_failure_migration_and_trial_cleanup(self):
        for ordinal in (0, 1):
            with self.subTest(ordinal=ordinal):
                FakeEngine.instances = []
                FakeEngine.fail_ready = ordinal
                with self.assertRaisesRegex(RuntimeError, "ready failure"):
                    self.run_main("--trials", "1")
                self.assert_stopped_and_removed()

    def test_seed_failure_cleanup(self):
        with patch.object(benchmark, "seed_fixture", side_effect=sqlite3.OperationalError("seed failure")):
            with self.assertRaises(sqlite3.OperationalError):
                self.run_main()
        self.assert_stopped_and_removed()

    def test_seed_exception_closes_sqlite_and_rolls_back(self):
        engine = FakeEngine(self.binary, self.root, False)
        engine.close()
        database = self.root / "FileID/fileid.sqlite"
        with closing(sqlite3.connect(database)) as db, db:
            db.execute("CREATE TRIGGER fail_seed BEFORE INSERT ON files WHEN NEW.id=50 BEGIN SELECT RAISE(ABORT, 'seed failure'); END")
        closed = []

        class TrackedConnection(sqlite3.Connection):
            def close(self):
                super().close()
                closed.append(True)

        connect = sqlite3.connect
        with patch.object(benchmark.sqlite3, "connect", side_effect=lambda path: connect(path, factory=TrackedConnection)):
            with self.assertRaisesRegex(sqlite3.IntegrityError, "seed failure"):
                benchmark.seed_fixture(self.root, 100)
        self.assertEqual(closed, [True])
        with closing(connect(database)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM files").fetchone()[0], 0)

    def test_fixture_directory_reuse_rejected_before_engine_start(self):
        reused = self.root / "reused"
        reused.mkdir()
        sentinel = reused / "original"
        sentinel.write_text("preserved")

        class ReusedDirectory:
            def __enter__(self):
                return str(reused)

            def __exit__(self, *args):
                return False

        with patch.object(benchmark.tempfile, "TemporaryDirectory", return_value=ReusedDirectory()):
            with self.assertRaisesRegex(RuntimeError, "directory reuse"):
                self.run_main()
        self.assertEqual(FakeEngine.instances, [])
        self.assertEqual(sentinel.read_text(), "preserved")

    def test_first_priming_warmup_and_measured_request_failure_cleanup(self):
        for index in (1, 2, 6, 16):
            with self.subTest(index=index):
                FakeEngine.instances = []
                FakeEngine.fail_request = (1, index)
                with self.assertRaisesRegex(RuntimeError, "request failure"):
                    self.run_main("--trials", "1")
                self.assert_stopped_and_removed()

    def test_malformed_required_response_and_hit_fields_reject_results(self):
        responses = [None, {}]
        for field in ("requestID", "status", "hits", "chapters", "jobs"):
            missing = deepcopy(VALID_RESPONSE)
            del missing[field]
            responses.append(missing)
            wrong_type = deepcopy(VALID_RESPONSE)
            wrong_type[field] = 42 if field in ("requestID", "status") else {}
            responses.append(wrong_type)
        for status in ("error", "unknown", True):
            response = deepcopy(VALID_RESPONSE)
            response["status"] = status
            responses.append(response)
        for hits in ([], "hit", [None], [{}]):
            response = deepcopy(VALID_RESPONSE)
            response["hits"] = hits
            responses.append(response)
        for field in ("fileID", "path", "kind", "text"):
            missing = deepcopy(VALID_RESPONSE)
            del missing["hits"][0][field]
            responses.append(missing)
            wrong_values = (True, 42.0, "42", None) if field == "fileID" else (42, False, None)
            for value in wrong_values:
                response = deepcopy(VALID_RESPONSE)
                response["hits"][0][field] = value
                responses.append(response)
        for response in responses:
            with self.subTest(response=response):
                FakeEngine.instances = []
                FakeEngine.response = response
                output = self.root / "rejected.json"
                with self.assertRaises(RuntimeError):
                    self.run_main("--trials", "1", "--output", str(output))
                self.assertFalse(output.exists())
                self.assert_stopped_and_removed()

    def test_unknown_response_and_hit_keys_remain_ignored(self):
        FakeEngine.response["futureRoot"] = {"value": "ignored"}
        FakeEngine.response["hits"][0]["futureHit"] = True
        self.run_main("--trials", "1", "--warmups", "0")
        self.assert_stopped_and_removed()

    def test_binary_change_rejects_result(self):
        FakeEngine.mutate_binary = True
        with self.assertRaisesRegex(RuntimeError, "binary changed"):
            self.run_main("--trials", "1")
        self.assert_stopped_and_removed()

    def test_serialization_failure_after_stop_before_cleanup(self):
        dumps = json.dumps

        def fail_result(value, **kwargs):
            if isinstance(value, dict) and "schema_version" in value:
                self.assertTrue(all(engine.closed for engine in FakeEngine.instances))
                self.assertTrue(FakeEngine.instances[-1].directory.exists())
                raise ValueError("serialization failure")
            return dumps(value, **kwargs)

        output = self.root / "failed.json"
        with patch.object(benchmark.json, "dumps", side_effect=fail_result), self.assertRaisesRegex(ValueError, "serialization failure"):
            self.run_main("--trials", "1", "--output", str(output))
        self.assertFalse(output.exists())
        self.assert_stopped_and_removed()


if __name__ == "__main__":
    unittest.main()

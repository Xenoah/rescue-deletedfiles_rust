"""CLI and real stdio MCP integration checks. Uses disposable, synthetic files."""
from pathlib import Path
from queue import Queue
import argparse
import hashlib
import json
import subprocess
import tempfile
import threading
import time
from make_fixture import build, MFT_LCN, RECORD, SECTOR


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def run(binary, *args, expected=0):
    p = subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True,
                       encoding="utf-8", timeout=30)
    check(p.returncode == expected, f"exit {p.returncode}, expected {expected}: {args}\n{p.stdout}\n{p.stderr}")
    return json.loads(p.stdout) if p.stdout.strip() else p.stderr


class Client:
    def __init__(self, binary, source, work, output):
        self.process = subprocess.Popen(
            [str(binary), "mcp", "--stdio", "--allow-source", str(source),
             "--work-dir", str(work), "--allow-output-root", str(output)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", bufsize=1)
        self.queue = Queue()
        self.next_id = 0
        threading.Thread(target=self.reader, daemon=True).start()

    def reader(self):
        for line in self.process.stdout:
            self.queue.put(json.loads(line))

    def rpc(self, method, params=None):
        self.next_id += 1
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next_id,
                                            "method": method, "params": params or {}}) + "\n")
        self.process.stdin.flush()
        result = self.queue.get(timeout=20)
        check(result["id"] == self.next_id, "JSON-RPC ID mismatch or leaked stdout log")
        return result

    def tool(self, name, arguments=None, error=False):
        result = self.rpc("tools/call", {"name": name, "arguments": arguments or {}})["result"]
        check(result.get("isError", False) == error, result)
        return result if error else result["structuredContent"]

    def wait(self, job):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            result = self.tool("get_job", {"job_id": job})
            if result["state"] not in ("running", "cancelling"):
                return result
            time.sleep(0.01)
        raise AssertionError("MCP job timed out")

    def close(self):
        self.process.stdin.close()
        code = self.process.wait(timeout=15)
        check(code == 0, self.process.stderr.read())


def main(binary):
    cases = 0
    with tempfile.TemporaryDirectory(prefix="rescue-e2e-") as temp:
        root = Path(temp)
        source = root / "source.raw"
        truth = build(source)
        original_hash = hashlib.sha256(source.read_bytes()).hexdigest()
        summary = run(binary, "scan", "--source", source, "--work-dir", root/"work", "--json")
        session_path = Path(summary["session"])
        check(summary["candidates"] == len(truth), summary)
        check(summary["invalid_records"] == 0, summary)
        candidates = run(binary, "results", "--session", session_path, "--limit", "100", "--json")["candidates"]
        by_id = {c["id"]: c for c in candidates}
        check(set(by_id) == set(truth), "live record or missing deleted record")
        check(by_id["mft-20-7"]["original_path"] == "写真/日本語.txt", "UTF-16 path reconstruction")
        check(by_id["mft-20-7"]["path_verified"], "valid parent reference")
        check(not by_id["mft-25-7"]["path_verified"], "reused parent must not be trusted")
        cases += 1
        result = run(binary, "recover", "--session", session_path, "--all", "--destination", root/"out", "--json")
        for item in result["results"]:
            check(item["status"] == "completed", item)
            path = Path(item["path"])
            check(path.parent.samefile(root/"out"), "path escaped destination")
            data = path.read_bytes()
            expected = truth[item["candidate_id"]]
            check(data.hex() == expected["hex"], f"byte mismatch: {item['candidate_id']}")
            check(item["sha256"] == expected["sha256"], "output SHA-256 mismatch")
        check(hashlib.sha256(source.read_bytes()).hexdigest() == original_hash, "source was modified")
        cases += len(truth)
        prior = {p.name: p.read_bytes() for p in (root/"out").iterdir()}
        run(binary, "recover", "--session", session_path, "--id", "mft-20-7", "--destination", root/"out", "--json")
        check(all((root/"out"/name).read_bytes() == data for name, data in prior.items()), "existing file overwritten")
        cases += 1
        run(binary, "recover", "--session", session_path, "--id", "mft-999-7", "--destination", root/"out", "--json", expected=2)
        run(binary, "results", "--session", session_path, "--limit", "0", "--json", expected=2)
        cases += 2

        broken = root/"broken.raw"
        build(broken, corrupt=True, unsupported=True)
        summary = run(binary, "scan", "--source", broken, "--work-dir", root/"bad-work", "--json", expected=5)
        check(summary["invalid_records"] == 1, "torn MFT record was not reported")
        results = run(binary, "results", "--session", summary["session"], "--json")["candidates"]
        check(sum(not c["supported"] for c in results) == 2, "unsupported data silently accepted")
        cases += 1

        # Persisted sessions are untrusted: tampering cannot direct reads to arbitrary extents.
        edited = json.loads(session_path.read_text(encoding="utf-8"))
        edited["candidates"][0]["record"] = 2**64-1
        edited_path = root/"edited.json"
        edited_path.write_text(json.dumps(edited), encoding="utf-8")
        result = run(binary, "recover", "--session", edited_path, "--id", edited["candidates"][0]["id"],
                     "--destination", root/"edited-out", "--json", expected=5)
        check(result["results"][0]["path"] is None, "invalid record produced a file")
        cases += 1

        with source.open("r+b") as f:
            f.seek(MFT_LCN*SECTOR + 20*RECORD + 70)
            f.write(b"changed")
        run(binary, "recover", "--session", session_path, "--all", "--destination", root/"changed-out", "--json", expected=4)
        cases += 1
        # Restore a fresh fixture for MCP checks.
        mcp_source = root/"mcp.raw"
        build(mcp_source)
        outside = root/"outside"
        outside.mkdir()
        client = Client(binary, mcp_source, root/"mcp-work", root/"mcp-out")
        try:
            check("error" in client.rpc("tools/list"), "tools available before initialization")
            init = client.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                            "clientInfo": {"name": "rescue-integration", "version": "1"}})
            check(init["result"]["protocolVersion"] == "2025-11-25", "protocol negotiation")
            check(len(client.rpc("tools/list")["result"]["tools"]) == 7, "tool list")
            allowed = client.tool("list_sources")["sources"]
            check(len(allowed) == 1 and Path(allowed[0]).samefile(mcp_source), "source scope")
            client.tool("start_scan", {"source": str(source), "request_id": "denied"}, error=True)
            request = {"source": str(mcp_source), "request_id": "scan-1"}
            first = client.tool("start_scan", request)
            check(client.tool("start_scan", request)["job_id"] == first["job_id"], "duplicate job")
            client.tool("start_scan", {"source": str(source), "request_id": "scan-1"}, error=True)
            job = client.wait(first["job_id"])
            check(job["state"] == "completed", job)
            sid = job["result"]["session_id"]
            page = client.tool("list_candidates", {"session_id": sid, "limit": 2, "offset": 1})
            check(len(page["candidates"]) == 2 and page["total"] == len(truth), "pagination")
            client.tool("recover_files", {"session_id": sid, "ids": ["mft-20-7"],
                "destination": str(outside), "request_id": "bad-write"}, error=True)
            task = client.tool("recover_files", {"session_id": sid, "ids": list(truth),
                "destination": str(root/"mcp-out"), "request_id": "recover-1"})
            job = client.wait(task["job_id"])
            check(job["state"] == "completed", job)
            for item in job["result"]["results"]:
                check(hashlib.sha256(Path(item["path"]).read_bytes()).hexdigest() == truth[item["candidate_id"]]["sha256"], "MCP/CLI mismatch")
            check(not list(outside.iterdir()), "unauthorized destination was written")
            cases += 9
        finally:
            client.close()
        large = root/"cancellation.raw"
        build(large, records=100_000)
        client = Client(binary, large, root/"cancel-work", root/"cancel-out")
        try:
            client.rpc("initialize", {"protocolVersion": "2025-11-25"})
            started = client.tool("start_scan", {"source": str(large), "request_id": "cancel-scan"})
            client.tool("cancel_job", {"job_id": started["job_id"]})
            finished = client.wait(started["job_id"])
            check(finished["state"] == "cancelled", finished)
            report = client.tool("get_report", {"session_id": finished["result"]["session_id"]})
            check(report["cancelled"] and report["scanned_records"] < report["total_records"], "cancellation did not stop scanning")
            cases += 1
        finally:
            client.close()
    print(json.dumps({"status": "passed", "checks": cases, "scope": "synthetic NTFS + CLI + stdio MCP"}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    main(args.binary.resolve())

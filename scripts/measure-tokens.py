#!/usr/bin/env python3
"""Measure the size of sous-chef tool answers, to compare them with what an agent would
otherwise read (files, logs, `bitbake -e` output).

It starts `sous-chef serve` over stdio, calls each tool given on the command line, and
prints each answer's size in bytes and estimated tokens (bytes / 4), followed by the text
unless --sizes-only is given. It runs no BitBake and writes nothing except the index cache
under --cache (default: a temporary directory, removed at the end).

Usage:
    scripts/measure-tokens.py [--binary PATH] [--config FILE] [--cwd DIR] [--cache DIR]
                              [--sizes-only] TOOL 'ARGS-JSON' [TOOL 'ARGS-JSON' ...]

Example, from a build's working directory:
    scripts/measure-tokens.py --config sous-chef.toml --sizes-only \\
        var '{"name":"SRC_URI","recipe":"curl"}' \\
        recipe '{"name":"curl"}' \\
        recipe '{"name":"curl","show":"tree"}'

Compare each size with the files the same question would need, for example
`wc -c` of the recipe and the classes and includes `recipe show=tree` lists, or of a task
log under `${T}`. The README's "Token savings" section was produced this way.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

BYTES_PER_TOKEN = 4


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--binary", default="sous-chef", help="the sous-chef binary (default: on PATH)")
    ap.add_argument("--config", help="sous-chef.toml to pass with --config")
    ap.add_argument("--cwd", default=".", help="directory to start the server in (default: .)")
    ap.add_argument("--cache", help="XDG_CACHE_HOME for the index (default: a temporary directory)")
    ap.add_argument("--sizes-only", action="store_true", help="print sizes, not the answers")
    ap.add_argument("calls", nargs="+", help="TOOL 'ARGS-JSON' pairs")
    opts = ap.parse_args()
    if len(opts.calls) % 2:
        ap.error("calls come in pairs: TOOL 'ARGS-JSON'")
    calls = []
    for i in range(0, len(opts.calls), 2):
        try:
            args = json.loads(opts.calls[i + 1])
        except json.JSONDecodeError as e:
            ap.error(f"arguments for {opts.calls[i]} are not JSON: {e}")
        calls.append((opts.calls[i], args))

    cache = opts.cache or tempfile.mkdtemp(prefix="sous-chef-measure-")
    argv = [opts.binary] + (["--config", opts.config] if opts.config else []) + ["serve"]
    env = dict(os.environ, XDG_CACHE_HOME=cache)
    try:
        server = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, cwd=opts.cwd, env=env)
    except OSError as e:
        if not opts.cache:
            shutil.rmtree(cache, ignore_errors=True)
        sys.exit(f"cannot start {opts.binary}: {e}")

    def send(msg):
        server.stdin.write(json.dumps(msg) + "\n")
        server.stdin.flush()

    def receive(want_id):
        while True:
            line = server.stdout.readline()
            if not line:
                sys.exit("the server closed its output (run `sous-chef status` to see why)")
            msg = json.loads(line)
            if msg.get("id") == want_id:
                return msg

    total = 0
    try:
        send({"jsonrpc": "2.0", "id": 0, "method": "initialize",
              "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                         "clientInfo": {"name": "measure-tokens", "version": "1"}}})
        receive(0)
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        for n, (tool, args) in enumerate(calls, start=1):
            send({"jsonrpc": "2.0", "id": n, "method": "tools/call",
                  "params": {"name": tool, "arguments": args}})
            msg = receive(n)
            if "error" in msg:
                print(f"{tool} {json.dumps(args)}: error: {msg['error'].get('message')}")
                continue
            result = msg.get("result", {})
            text = "".join(c.get("text", "") for c in result.get("content", []))
            size = len(text.encode())
            total += size
            flag = " (tool error)" if result.get("isError") else ""
            print(f"{tool} {json.dumps(args)}: {size} bytes, ~{size // BYTES_PER_TOKEN} tokens{flag}")
            if not opts.sizes_only:
                print(text)
                print()
        print(f"total: {total} bytes, ~{total // BYTES_PER_TOKEN} tokens")
    finally:
        server.stdin.close()
        try:
            server.wait(timeout=30)
        except subprocess.TimeoutExpired:
            server.kill()
        if not opts.cache:
            shutil.rmtree(cache, ignore_errors=True)


if __name__ == "__main__":
    main()

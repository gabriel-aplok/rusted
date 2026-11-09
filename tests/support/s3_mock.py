#!/usr/bin/env python3
import http.server
import os
import sys
import urllib.parse
from datetime import datetime, timezone

root = os.path.abspath(sys.argv[1])
port_file = sys.argv[2]


def safe_path(key):
    parts = [part for part in key.split("/") if part]
    if any(part in ("..", ".") for part in parts):
        raise ValueError("unsafe key")
    return os.path.join(root, *parts)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_PUT(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        path = safe_path(self.path.lstrip("/").split("/", 1)[1])
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as handle:
            handle.write(body)
        self.send_response(200)
        self.end_headers()

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        if query.get("list-type") == ["2"]:
            self.list_objects(query.get("prefix", [""])[0])
            return
        try:
            path = safe_path(parsed.path.lstrip("/").split("/", 1)[1])
        except (IndexError, ValueError):
            self.send_response(400)
            self.end_headers()
            return
        if not os.path.isfile(path):
            self.send_response(404)
            self.end_headers()
            return
        with open(path, "rb") as handle:
            body = handle.read()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_DELETE(self):
        try:
            path = safe_path(self.path.lstrip("/").split("/", 1)[1])
        except (IndexError, ValueError):
            self.send_response(400)
            self.end_headers()
            return
        if os.path.isfile(path):
            os.remove(path)
        self.send_response(204)
        self.end_headers()

    def list_objects(self, prefix):
        entries = []
        for directory, _, files in os.walk(root):
            for name in files:
                path = os.path.join(directory, name)
                key = os.path.relpath(path, root).replace(os.sep, "/")
                if not key.startswith(prefix):
                    continue
                modified = datetime.fromtimestamp(os.path.getmtime(path), timezone.utc)
                entries.append(
                    "<Contents><Key>{}</Key><Size>{}</Size><LastModified>{}</LastModified></Contents>".format(
                        key,
                        os.path.getsize(path),
                        modified.strftime("%Y-%m-%dT%H:%M:%S.000Z"),
                    )
                )
        body = (
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>"
            "<ListBucketResult><IsTruncated>false</IsTruncated>"
            + "".join(entries)
            + "</ListBucketResult>"
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/xml")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as handle:
    handle.write(str(server.server_address[1]))
server.serve_forever()

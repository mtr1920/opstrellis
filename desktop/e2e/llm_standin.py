"""Local OpenAI-compatible stand-in for E2E tests (real LLM providers are unreachable here).
Cleanup requests echo the dictation unchanged; cleanup with an auto action and command requests
return "TRANSFORMED: <text>" so the test can see which step ran; a style note containing
SHOUT_DICTATION makes cleanup return the dictation in capitals (an LLM that recases everything). With a second argument, each
request's system and user prompts are appended to that file (one JSON object per line) so tests can count
requests and check prompts. STANDIN_DELAY_MS adds a fixed delay per request (a network round trip)
for latency benchmarks."""
import json, os, re, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        user = body["messages"][-1]["content"]
        system = body["messages"][0]["content"]
        if len(sys.argv) > 2:
            with open(sys.argv[2], "a") as f:
                f.write(json.dumps({"system": system, "user": user}) + "\n")
        sel = re.search(r"<selected_text>\n(.*)\n</selected_text>", user, re.S)
        dic = re.search(r"<dictation>\n(.*)\n</dictation>", user, re.S)
        if "KEEP_ACTION_INDENT" in system + user:
            content = "- Plan\n  - Use Postgres\n\n```python\nif ready:\n    run()\n```"
        elif dic and "SHOUT_DICTATION" in system:
            content = dic.group(1).upper()
        elif sel:
            content = f"TRANSFORMED: {sel.group(1)}"
        elif dic and "Then rewrite the cleaned-up text" in system:
            content = f"TRANSFORMED: {dic.group(1)}"
        else:
            content = dic.group(1) if dic else user
        time.sleep(int(os.environ.get("STANDIN_DELAY_MS", "0")) / 1000)
        out = json.dumps({"choices": [{"message": {"role": "assistant", "content": content}}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *_):
        pass


HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()

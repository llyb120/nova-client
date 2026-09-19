"""Opt-in local model worker for Polaris. Models are loaded from local safetensors only.
No automatic download, remote code, source logging, browser access or arbitrary paths.
"""
from __future__ import annotations
import argparse
import hmac
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import BoundedSemaphore, Lock
import time

MAX_BODY = 256 * 1024

class ModelWorker:
    def __init__(self, args: argparse.Namespace):
        import torch
        from transformers import AutoModel, AutoModelForSequenceClassification, AutoTokenizer
        torch.set_num_threads(args.threads)
        self.torch = torch
        self.identity = args.identity
        self.max_tokens = args.max_tokens
        self.lock = Lock()
        self.calls = {"query": 0, "passage": 0, "rerank": 0}
        self.tokenizer = AutoTokenizer.from_pretrained(args.model_path, local_files_only=True, trust_remote_code=False)
        self.model = AutoModel.from_pretrained(args.model_path, local_files_only=True, trust_remote_code=False, use_safetensors=True).eval()
        self.ranker = self.rank_tokenizer = None
        if args.rerank_path:
            self.rank_tokenizer = AutoTokenizer.from_pretrained(args.rerank_path, local_files_only=True, trust_remote_code=False)
            self.ranker = AutoModelForSequenceClassification.from_pretrained(args.rerank_path, local_files_only=True, trust_remote_code=False, use_safetensors=True).eval()
        self.admission = BoundedSemaphore(2)

    def run(self, path: str, data: dict) -> dict:
        if data.get("model") != self.identity:
            raise ValueError("model identity mismatch")
        texts = data.get("texts")
        if not isinstance(texts, list) or not 1 <= len(texts) <= 16 or any(not isinstance(s, str) or len(s) > 5000 for s in texts):
            raise ValueError("texts must contain 1..16 strings of at most 5000 characters")
        with self.lock, self.torch.inference_mode():
            if path == "/embed":
                kind = data.get("kind")
                if kind not in ("query", "passage"):
                    raise ValueError("invalid embedding kind")
                batch = self.tokenizer([kind + ": " + s for s in texts], max_length=self.max_tokens, padding=True, truncation=True, return_tensors="pt")
                hidden = self.model(**batch).last_hidden_state
                mask = batch["attention_mask"].unsqueeze(-1).bool()
                vectors = hidden.masked_fill(~mask, 0.0).sum(dim=1) / mask.sum(dim=1).clamp(min=1)
                vectors = self.torch.nn.functional.normalize(vectors, p=2, dim=1)
                self.calls[kind] += len(texts)
                return {"model": self.identity, "vectors": vectors.tolist(), "maxTokens": self.max_tokens}
            if path == "/rerank":
                query = data.get("query")
                if self.ranker is None:
                    raise ValueError("reranker is not configured")
                if not isinstance(query, str) or len(query) > 1024:
                    raise ValueError("invalid query")
                batch = self.rank_tokenizer([query] * len(texts), texts, max_length=self.max_tokens, padding=True, truncation=True, return_tensors="pt")
                scores = self.ranker(**batch).logits.reshape(-1)
                if scores.numel() != len(texts):
                    raise ValueError("reranker must emit one score per passage")
                self.calls["rerank"] += len(texts)
                return {"model": self.identity, "scores": scores.tolist()}
            raise ValueError("unknown endpoint")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-path", required=True, type=Path)
    parser.add_argument("--rerank-path", type=Path)
    parser.add_argument("--identity", required=True, help="Pinned encoder/reranker revisions and configuration")
    parser.add_argument("--port", default=48771, type=int)
    parser.add_argument("--threads", default=2, type=int)
    parser.add_argument("--max-tokens", default=256, type=int)
    args = parser.parse_args()
    if not 1 <= args.threads <= 16 or not 64 <= args.max_tokens <= 512:
        parser.error("threads must be 1..16; max-tokens must be 64..512")
    token = os.environ.get("NOVA_POLARIS_SEMANTIC_TOKEN", "")
    if len(token) < 24:
        parser.error("set a random NOVA_POLARIS_SEMANTIC_TOKEN with at least 24 characters")
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["TOKENIZERS_PARALLELISM"] = "false"
    started = time.perf_counter()
    worker = ModelWorker(args)
    loaded = (time.perf_counter() - started) * 1000

    class Handler(BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(40)

        def log_message(self, *_args):
            pass

        def reply(self, code: int, value: dict):
            body = json.dumps(value, ensure_ascii=False, allow_nan=False).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def authorized(self):
            return not self.headers.get("Origin") and hmac.compare_digest(self.headers.get("Authorization", ""), "Bearer " + token)

        def do_GET(self):
            if not self.authorized():
                self.reply(403, {"error": "forbidden"})
            elif self.path == "/health":
                self.reply(200, {"model": worker.identity, "rerank": worker.ranker is not None, "loadMs": loaded, "calls": worker.calls.copy(), "threads": args.threads, "maxTokens": args.max_tokens})
            else:
                self.reply(404, {"error": "unknown endpoint"})

        def do_POST(self):
            if not self.authorized():
                self.reply(403, {"error": "forbidden"}); return
            if self.path not in ("/embed", "/rerank"):
                self.reply(404, {"error": "unknown endpoint"}); return
            if not worker.admission.acquire(blocking=False):
                self.reply(429, {"error": "model worker busy"}); return
            try:
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= MAX_BODY or self.headers.get_content_type() != "application/json":
                    self.reply(400, {"error": "invalid request size or type"}); return
                raw = self.rfile.read(size)
                if len(raw) != size:
                    raise ValueError("incomplete body")
                data = json.loads(raw)
                if not isinstance(data, dict):
                    raise ValueError("object required")
                self.reply(200, worker.run(self.path, data))
            except (ValueError, TypeError, KeyError):
                self.reply(400, {"error": "invalid model request"})
            except Exception:
                self.reply(503, {"error": "model inference failed"})
            finally:
                worker.admission.release()

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    print(json.dumps({"ready": True, "port": server.server_port, "model": worker.identity, "loadMs": loaded}), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()

if __name__ == "__main__":
    main()

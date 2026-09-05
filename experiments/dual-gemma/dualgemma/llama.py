"""Thin llama-server HTTP client. Stdlib only, no dependencies."""

import json
import time
import urllib.error
import urllib.request


class LlamaError(RuntimeError):
    pass


class Llama:
    """One llama-server instance, addressed over HTTP."""

    def __init__(self, base_url, name, think=True):
        self.base_url = base_url.rstrip("/")
        self.name = name
        # Whether the chat template opens a thinking channel. Decide ONCE per
        # session: the template injects the think token at the very top of the
        # first system turn, so flipping it mid-session moves the entire token
        # prefix and invalidates the whole KV cache (measured: 2967ms re-prefill
        # vs ~250ms warm).
        self.think = think
        self.calls = 0
        self.seconds = 0.0
        self.prompt_tokens = 0
        self.completion_tokens = 0
        self.truncated = 0

    def _post(self, path, payload, timeout=180):
        req = urllib.request.Request(
            self.base_url + path,
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
        )
        started = time.time()
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                body = json.loads(resp.read())
        except urllib.error.URLError as exc:
            raise LlamaError("%s at %s is unreachable: %s" % (self.name, self.base_url, exc))
        finally:
            self.seconds += time.time() - started
            self.calls += 1
        return body

    def health(self):
        try:
            with urllib.request.urlopen(self.base_url + "/health", timeout=3) as resp:
                return json.loads(resp.read()).get("status") == "ok"
        except Exception:
            return False

    def chat(self, messages, temperature=0.0, max_tokens=1024, stop=None):
        """OpenAI-compatible chat. Uses the model's own chat template.

        Gemma 4 is a thinking model: it spends tokens in `reasoning_content`
        before writing any `content`. Too small a budget truncates it inside
        the reasoning and hands back an EMPTY string with
        `finish_reason: length` -- the model did the work and never got to
        state the conclusion. Budget generously, and if it still runs out,
        fall back to whatever it managed to reason so the caller can salvage
        a directive rather than seeing silence.
        """
        payload = {
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "cache_prompt": True,
        }
        if not self.think:
            payload["chat_template_kwargs"] = {"enable_thinking": False}
        if stop:
            payload["stop"] = stop
        body = self._post("/v1/chat/completions", payload)
        usage = body.get("usage") or {}
        self.prompt_tokens += usage.get("prompt_tokens", 0)
        self.completion_tokens += usage.get("completion_tokens", 0)

        timings = body.get("timings") or {}
        details = (usage.get("prompt_tokens_details") or {})
        self.last = {
            "prefill_ms": round(timings.get("prompt_ms") or 0),
            "decode_tps": round(timings.get("predicted_per_second") or 0, 1),
            "cached": details.get("cached_tokens", 0),
            "prompt": usage.get("prompt_tokens", 0),
            "gen": usage.get("completion_tokens", 0),
        }

        choice = body["choices"][0]
        message = choice.get("message") or {}
        content = message.get("content") or ""
        if choice.get("finish_reason") == "length":
            self.truncated += 1
        if not content:
            content = message.get("reasoning_content") or ""
        return content

    def chat_stream(self, messages, temperature=0.0, max_tokens=1024, on_delta=None):
        """Streaming chat. Yields nothing; returns the final content string.

        `on_delta(kind, text)` fires per chunk with kind "reason" or
        "content". TTFT to the caller becomes the first delta, not the end
        of the whole completion. Counters update from the final chunk's
        usage/timings when the server includes them.
        """
        payload = {
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "cache_prompt": True,
            "stream": True,
            "stream_options": {"include_usage": True},
        }
        if not self.think:
            payload["chat_template_kwargs"] = {"enable_thinking": False}

        req = urllib.request.Request(
            self.base_url + "/v1/chat/completions",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
        )
        content, reasoning = [], []
        started = time.time()
        self.calls += 1
        try:
            with urllib.request.urlopen(req, timeout=180) as resp:
                for raw in resp:
                    line = raw.decode("utf-8", "replace").strip()
                    if not line.startswith("data: "):
                        continue
                    data = line[len("data: "):]
                    if data == "[DONE]":
                        break
                    chunk = json.loads(data)

                    usage = chunk.get("usage")
                    if usage:
                        self.prompt_tokens += usage.get("prompt_tokens", 0)
                        self.completion_tokens += usage.get("completion_tokens", 0)
                        details = usage.get("prompt_tokens_details") or {}
                        timings = chunk.get("timings") or {}
                        self.last = {
                            "prefill_ms": round(timings.get("prompt_ms") or 0),
                            "decode_tps": round(timings.get("predicted_per_second") or 0, 1),
                            "cached": details.get("cached_tokens", 0),
                            "prompt": usage.get("prompt_tokens", 0),
                            "gen": usage.get("completion_tokens", 0),
                        }

                    choices = chunk.get("choices") or []
                    if not choices:
                        continue
                    delta = choices[0].get("delta") or {}
                    piece = delta.get("reasoning_content")
                    if piece:
                        reasoning.append(piece)
                        if on_delta:
                            on_delta("reason", piece)
                    piece = delta.get("content")
                    if piece:
                        content.append(piece)
                        if on_delta:
                            on_delta("content", piece)
        except urllib.error.URLError as exc:
            raise LlamaError("%s at %s is unreachable: %s" % (self.name, self.base_url, exc))
        finally:
            self.seconds += time.time() - started

        return "".join(content) or "".join(reasoning)

    def complete(self, prompt, temperature=0.0, max_tokens=256, stop=None):
        """Raw completion, bypassing every chat template.

        FunctionGemma needs this: its prompt format is a hand-built token
        sequence that no generic Jinja template will reproduce.
        """
        payload = {
            "prompt": prompt,
            "temperature": temperature,
            "n_predict": max_tokens,
            "cache_prompt": True,
        }
        if stop:
            payload["stop"] = stop
        body = self._post("/completion", payload)
        timings = body.get("timings") or {}
        self.prompt_tokens += int(timings.get("prompt_n", 0))
        self.completion_tokens += int(timings.get("predicted_n", 0))
        return body.get("content", "")

    def stats(self):
        return {
            "model": self.name,
            "calls": self.calls,
            "seconds": round(self.seconds, 2),
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "truncated": self.truncated,
        }

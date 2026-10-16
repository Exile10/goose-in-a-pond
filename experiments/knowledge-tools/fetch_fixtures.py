#!/usr/bin/env python3
"""Freeze the Wikipedia extracts every read case needs, once, so runs compare designs and not
the network. Same MediaWiki call `KnowledgeMcpServer::fetch_article_summary` makes."""

import json
import os
import sys
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures", "wiki")
UA = "goose-in-a-pond/0.1 (GIAP knowledge-tools bench; https://github.com/jarida-io/goose-in-a-pond)"


def get(url):
    req = urllib.request.Request(url, headers={"user-agent": UA})
    with urllib.request.urlopen(req, timeout=20) as resp:
        return json.loads(resp.read())


def fetch(title):
    url = (
        "https://en.wikipedia.org/w/api.php?action=query&titles=%s"
        "&prop=extracts|info&explaintext=1&inprop=url&format=json&redirects=1"
        % urllib.parse.quote(title)
    )
    pages = get(url)["query"]["pages"]
    page = next(iter(pages.values()))
    if "missing" in page or not page.get("extract"):
        return None
    return {"title": page["title"], "url": page.get("fullurl", ""), "extract": page["extract"]}


def main():
    os.makedirs(OUT, exist_ok=True)
    cases = json.load(open(os.path.join(HERE, "cases.json")))["cases"]
    for title in sorted({c["wiki"] for c in cases if c.get("wiki")}):
        path = os.path.join(OUT, title.replace("/", "_") + ".json")
        if os.path.exists(path):
            continue
        page = fetch(title)
        if page is None:
            print("MISSING:", title, file=sys.stderr)
            continue
        json.dump(page, open(path, "w"), ensure_ascii=False, indent=1)
        print("%-55s %6d chars" % (page["title"], len(page["extract"])))


if __name__ == "__main__":
    main()

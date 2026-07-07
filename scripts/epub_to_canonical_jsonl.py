#!/usr/bin/env python3
"""
Convert the CSB Holy Bible EPUB to canonical-jsonl format.

Each output line is a JSON object representing one verse:

    {"source_profile":"bible","work_id":"CSB","version_id":"digital-edition-2017",
     "language":"en",
     "components":[
         {"level":"book","value":"John","ordinal":43},
         {"level":"chapter","value":"3","ordinal":3},
         {"level":"verse","value":"16","ordinal":16}
     ],
     "display_citation":"John 3:16",
     "text":"For God loved the world...",
     "metadata":{"section_heading":"...","testament":"NT"}}

Usage:
    python3 epub_to_canonical_jsonl.py [EPUB_PATH] [OUTPUT_PATH]

Defaults:
    EPUB_PATH = /home/obj/project/github/RyderFreeman4Logos/CSB_Holy_Bible_Digital_Edition_2017_Holman/CSB_Holy_Bible_Digital_Edition_2017_Holman.epub
    OUTPUT_PATH = /tmp/csb_bible.jsonl
"""

import html
import json
import os
import re
import sys
import zipfile

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

DEFAULT_EPUB = (
    "/home/obj/project/github/RyderFreeman4Logos/"
    "CSB_Holy_Bible_Digital_Edition_2017_Holman/"
    "CSB_Holy_Bible_Digital_Edition_2017_Holman.epub"
)
DEFAULT_OUTPUT = "/tmp/csb_bible.jsonl"

# Regexes -------------------------------------------------------------------

# Match a book content file: CSB01_Genesis.xhtml  (not _nav, not CSB00 frontmatter)
# Ordinal is 01-66; reject CSB00_* frontmatter.
RE_BOOK_FILE = re.compile(r"^OEBPS/Text/CSB(\d{2})_([A-Za-z0-9]+)\.xhtml$")

# Verse marker span: <span epub:type="z3998:verse" id="start-Book.Ch.Verse"></span>
RE_VERSE_MARKER = re.compile(
    r'<span\s+epub:type="z3998:verse"\s+id="start-([^"]+)"></span>'
)

# Any h1 opening tag (to locate section headings + bookname headings)
RE_H1_OPEN = re.compile(r"<h1\b[^>]*>")

# Footnote / cross-reference sections to discard entirely
RE_FN_SECTION = re.compile(
    r'<section\s+class="fnSection"[^>]*>.*?</section>', re.DOTALL
)
RE_XRF_SECTION = re.compile(
    r'<section\s+class="xrfSection"[^>]*>.*?</section>', re.DOTALL
)

# noteref anchors (footnotes + cross-references inline) — remove with content.
RE_NOTEREF = re.compile(
    r'<a\s+epub:type="noteref"[^>]*>.*?</a>', re.DOTALL
)
# After removing noterefs, a comma that was separating two adjacent markers
# (e.g. "...earth.<a>A</a>, <a>b</a>" → "...earth., ") is left dangling.
# Remove the "marker-comma-space-then-nothing" artifact: a comma (optionally
# preceded/followed by whitespace) that sits between where a noteref was.
# We handle this in clean_verse_text() via targeted post-processing.

# chapter-number / verse-number spans — remove with content
RE_CHAPTER_NUM = re.compile(
    r'<span\s+class="chapterNumber"[^>]*>.*?</span>', re.DOTALL
)
RE_VERSE_NUM = re.compile(
    r'<span\s+class="verseNumber"[^>]*>.*?</span>', re.DOTALL
)

# Empty verse-marker spans (safety: also strip mid-chunk leftovers)
RE_VERSE_SPAN = re.compile(
    r'<span\s+epub:type="z3998:verse"[^>]*>\s*</span>'
)

# Block-level closing tags → ensure a space so words don't merge
RE_BLOCK_CLOSE = re.compile(r"</(?:p|section|blockquote|div|h[1-6])>")

# Any remaining tag
RE_ANY_TAG = re.compile(r"<[^>]+>")

# Whitespace collapse
RE_WS = re.compile(r"\s+")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def ordinal_to_testament(ordinal: int) -> str:
    """Books 1-39 = OT, 40-66 = NT."""
    return "OT" if ordinal <= 39 else "NT"


def extract_body(raw: str) -> str:
    """Return only the <body>…</body> portion of an XHTML file."""
    m = re.search(r"<body[^>]*>(.*)</body>", raw, re.DOTALL)
    return m.group(1) if m else raw


def strip_reference_sections(body: str) -> str:
    """Remove footnote, cross-reference, and structural navigation sections so
    their text is ignored during verse extraction.

    Strips:
      - <section class="fnSection">…</section>   (footnotes)
      - <section class="xrfSection">…</section>   (cross-references)
      - <section epub:type="chapter" id="csb-…">  (empty chapter-boundary markers)
      - <h1 class="bookname">…</h1>               (book name + nav arrows)
      - <h4>…</h4>                                (chapter titles like "Psalm 24")
      - <h2>…</h2>                                (chapter subtitles)
    """
    body = RE_FN_SECTION.sub("", body)
    body = RE_XRF_SECTION.sub("", body)
    # Empty chapter-boundary section markers (self-closing or empty)
    body = re.sub(
        r'<section\s+epub:type="chapter"[^>]*>\s*</section>', "", body
    )
    # Bookname headings (contain nav arrows + book name — not verse text)
    body = re.sub(
        r'<h1\s+class="bookname">.*?</h1>', "", body, flags=re.DOTALL
    )
    # Chapter titles in <h4> (e.g. "Psalm 24") and subtitles in <h2>
    body = re.sub(r"<h4\b[^>]*>.*?</h4>", "", body, flags=re.DOTALL)
    body = re.sub(r"<h2\b[^>]*>.*?</h2>", "", body, flags=re.DOTALL)
    return body


def clean_verse_text(raw_html: str) -> str:
    """Strip tags, footnote refs, and entities from a verse's raw HTML chunk."""
    text = RE_NOTEREF.sub("", raw_html)
    text = RE_CHAPTER_NUM.sub("", text)
    text = RE_VERSE_NUM.sub("", text)
    text = RE_VERSE_SPAN.sub("", text)
    # Insert space at block boundaries so concatenated text stays separated.
    text = RE_BLOCK_CLOSE.sub(" ", text)
    # Remove every remaining tag (smallcaps, redletter, italic, etc. — keep text)
    text = RE_ANY_TAG.sub("", text)
    # Decode HTML entities (&#160; → space, &#8217; → ’, &amp; → &, …)
    text = html.unescape(text)
    # Collapse whitespace
    text = RE_WS.sub(" ", text).strip()
    # Remove trailing comma/period artifacts left by stripped footnote markers.
    # CSB typography separates adjacent footnote/cross-ref letters with ", "
    # so removing markers can leave dangling punctuation like:
    #   "earth., "  → period + comma-separator remnant  (fix → "earth.")
    #   "place,, ," → multiple comma-separator remnants (fix → "place,")
    # Rule: only strip when the trailing punctuation run looks like an artifact
    # (contains 2+ commas, or a period immediately followed by comma+space).
    # A single trailing comma is preserved (it's a legitimate mid-sentence
    # comma in the verse, e.g. "...the little owls, cormorants, ...").
    # 1) "word., " or "word.," → "word."
    text = re.sub(r"\.\s*,+\s*$", ".", text)
    # 2) Trailing run with 2+ commas (e.g. ",,", ",,,", ", ,") → strip all
    text = re.sub(r"(?:,\s*){2,}$", "", text)
    return text.strip()


def get_title(raw: str) -> str:
    """Extract the <title> tag content (used for the display book name)."""
    m = re.search(r"<title>(.*?)</title>", raw, re.DOTALL)
    return RE_WS.sub(" ", m.group(1)).strip() if m else ""


def parse_book_file(raw: str, ordinal: int, title: str, testament: str):
    """
    Parse a single book XHTML file and yield verse dicts.

    Each verse dict has keys: components, display_citation, text, metadata.
    """
    body = extract_body(raw)
    body = strip_reference_sections(body)

    # Build a combined list of "events" in document order:
    #   ('heading', position, heading_text)
    #   ('verse',   position, (book, ch, vs, marker_id))
    events = []

    # Section headings: <h1> that are NOT the bookname heading.
    for m in RE_H1_OPEN.finditer(body):
        start = m.start()
        # Find matching </h1>
        end_m = re.search(r"</h1>", body[m.end():])
        if not end_m:
            continue
        inner = body[m.end():m.end() + end_m.start()]
        # Bookname headings have class="bookname" — skip those.
        if 'class="bookname"' in m.group(0) or "bookname" in m.group(0):
            continue
        heading_text = clean_verse_text(inner)  # reuse tag-stripping
        heading_text = heading_text.strip()
        if heading_text:
            # Remove leading/trailing arrows that appear in nav headings
            heading_text = heading_text.replace("→", "").replace("←", "").strip()
        if heading_text:
            events.append(("heading", start, heading_text))

    # Verse markers
    for m in RE_VERSE_MARKER.finditer(body):
        marker_id = m.group(1)  # e.g. "John.3.16" or "1_Corinthians.1.1"
        parts = marker_id.split(".")
        if len(parts) != 3:
            continue
        book_key, ch_str, vs_str = parts
        try:
            chapter = int(ch_str)
            verse = int(vs_str)
        except ValueError:
            continue
        events.append(("verse", m.start(), (book_key, chapter, verse, m.end())))

    events.sort(key=lambda e: e[1])

    # Walk events, tracking the current section heading, and for each verse
    # extract text from end of its marker span to the start of the next verse
    # marker OR the next heading, whichever comes first.
    current_heading = None
    results = []

    for idx, ev in enumerate(events):
        if ev[0] == "heading":
            current_heading = ev[2]
        elif ev[0] == "verse":
            _, _, (book_key, chapter, verse, marker_end) = ev
            # Determine where this verse's text chunk ends:
            #   the next event (verse marker or heading) after this one,
            #   whichever comes first. This prevents inter-chapter headings
            #   and navigation from leaking into the last verse of a chapter.
            text_end = len(body)
            for nxt in events[idx + 1:]:
                if nxt[1] > ev[1]:
                    text_end = nxt[1]
                    break
            raw_chunk = body[marker_end:text_end]
            text = clean_verse_text(raw_chunk)
            if not text:
                # Verse with no text (rare) — still emit so counts are complete.
                text = ""

            results.append({
                "components": [
                    {"level": "book", "value": title, "ordinal": ordinal},
                    {"level": "chapter", "value": str(chapter), "ordinal": chapter},
                    {"level": "verse", "value": str(verse), "ordinal": verse},
                ],
                "display_citation": f"{title} {chapter}:{verse}",
                "text": text,
                "metadata": {
                    "section_heading": current_heading,
                    "testament": testament,
                },
            })

    return results


# ---------------------------------------------------------------------------
# Main conversion
# ---------------------------------------------------------------------------

def convert(epub_path: str, output_path: str) -> dict:
    """Convert the EPUB to JSONL. Return summary stats."""
    if not os.path.exists(epub_path):
        raise FileNotFoundError(f"EPUB not found: {epub_path}")

    total_verses = 0
    book_counts = {}

    with zipfile.ZipFile(epub_path, "r") as zf:
        names = zf.namelist()
        # Collect book content files in ordinal order.
        book_files = []
        for name in names:
            m = RE_BOOK_FILE.match(name)
            if m:
                ordinal = int(m.group(1))
                # Skip CSB00_* frontmatter files (ordinal must be 1-66)
                if 1 <= ordinal <= 66:
                    book_files.append((ordinal, name))
        book_files.sort(key=lambda x: x[0])

        with open(output_path, "w", encoding="utf-8") as out:
            for ordinal, name in book_files:
                raw = zf.read(name).decode("utf-8", errors="replace")
                title = get_title(raw)
                testament = ordinal_to_testament(ordinal)

                verses = parse_book_file(raw, ordinal, title, testament)

                for v in verses:
                    record = {
                        "source_profile": "bible",
                        "work_id": "CSB",
                        "version_id": "digital-edition-2017",
                        "language": "en",
                        "components": v["components"],
                        "display_citation": v["display_citation"],
                        "text": v["text"],
                        "metadata": v["metadata"],
                    }
                    out.write(json.dumps(record, ensure_ascii=False) + "\n")
                    total_verses += 1

                book_counts[title] = len(verses)

    return {
        "total_verses": total_verses,
        "book_count": len(book_counts),
        "book_counts": book_counts,
        "output_path": output_path,
    }


def main():
    epub_path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_EPUB
    output_path = sys.argv[2] if len(sys.argv) > 2 else DEFAULT_OUTPUT

    stats = convert(epub_path, output_path)

    print(f"✓ Wrote {stats['total_verses']:,} verses from "
          f"{stats['book_count']} books to {stats['output_path']}")
    print("\nPer-book verse counts:")
    for book, count in stats["book_counts"].items():
        print(f"  {book:<20s} {count:>4d}")


if __name__ == "__main__":
    main()

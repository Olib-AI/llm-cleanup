#!/usr/bin/env python3
"""Generate the PDF fixtures used by the document-IR integration tests.

The fixtures are committed, because `cargo test` must never need Python. This script exists so
they can be regenerated reproducibly:

    pip install --break-system-packages reportlab pillow
    python3 scripts/generate_fixtures.py

Every file is written to `crates/cleanup-convert/tests/fixtures/`. Output is deterministic
(`rl_config.invariant`), so regenerating an unchanged script produces byte-identical PDFs and a
clean `git status`.

The five fixtures target one failure mode each:

* `multi_page_tables.pdf`      — a table that breaks across three pages with a repeated header,
                                 alternating fills, mixed column alignment, plus a second table
                                 with colspan/rowspan.
* `rich_media_layout.pdf`      — block and inline PNG/JPEG images, stroked vector rules, and
                                 tinted background callout boxes.
* `typographic_symbols.pdf`    — math symbols, Greek, extended Latin, arrows, indented code.
* `mixed_columns.pdf`          — single-column banner, two- and three-column body, endnotes.
* `ai_slop_samples.pdf`        — assistant preamble/disclaimer/filler around genuine content.
"""

from __future__ import annotations

import io
import os
import textwrap

from PIL import Image, ImageDraw
from reportlab import rl_config

rl_config.invariant = 1

from reportlab.lib import colors  # noqa: E402
from reportlab.lib.enums import TA_CENTER, TA_JUSTIFY, TA_RIGHT  # noqa: E402
from reportlab.lib.pagesizes import A4, LETTER  # noqa: E402
from reportlab.lib.styles import ParagraphStyle, getSampleStyleSheet  # noqa: E402
from reportlab.lib.units import inch  # noqa: E402
from reportlab.pdfbase import pdfmetrics  # noqa: E402
from reportlab.pdfbase.ttfonts import TTFont  # noqa: E402
from reportlab.platypus import (  # noqa: E402
    BaseDocTemplate,
    Frame,
    Image as PlatypusImage,
    PageBreak,
    PageTemplate,
    Paragraph,
    Spacer,
    Table,
    TableStyle,
)

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES = os.path.join(HERE, "crates", "cleanup-convert", "tests", "fixtures")

FONT_DIR = "/usr/share/fonts/truetype/dejavu"
REGULAR = os.path.join(FONT_DIR, "DejaVuSans.ttf")
BOLD = os.path.join(FONT_DIR, "DejaVuSans-Bold.ttf")
MONO = os.path.join(FONT_DIR, "DejaVuSansMono.ttf")


def register_fonts() -> None:
    """DejaVu covers the Greek/math/arrow range the typographic fixture needs."""
    pdfmetrics.registerFont(TTFont("Body", REGULAR))
    pdfmetrics.registerFont(TTFont("Body-Bold", BOLD))
    pdfmetrics.registerFont(TTFont("Mono", MONO))
    pdfmetrics.registerFontFamily(
        "Body", normal="Body", bold="Body-Bold", italic="Body", boldItalic="Body-Bold"
    )


def styles() -> dict[str, ParagraphStyle]:
    base = getSampleStyleSheet()
    return {
        "title": ParagraphStyle(
            "Title",
            parent=base["Title"],
            fontName="Body-Bold",
            fontSize=24,
            leading=29,
            spaceAfter=14,
        ),
        "h1": ParagraphStyle(
            "H1", parent=base["Heading1"], fontName="Body-Bold", fontSize=17, leading=22
        ),
        "h2": ParagraphStyle(
            "H2", parent=base["Heading2"], fontName="Body-Bold", fontSize=13, leading=17
        ),
        "body": ParagraphStyle(
            "Body", parent=base["BodyText"], fontName="Body", fontSize=10, leading=14
        ),
        "cell": ParagraphStyle(
            "Cell", parent=base["BodyText"], fontName="Body", fontSize=9, leading=12
        ),
        "cell_right": ParagraphStyle(
            "CellRight",
            parent=base["BodyText"],
            fontName="Body",
            fontSize=9,
            leading=12,
            alignment=TA_RIGHT,
        ),
        "cell_center": ParagraphStyle(
            "CellCenter",
            parent=base["BodyText"],
            fontName="Body",
            fontSize=9,
            leading=12,
            alignment=TA_CENTER,
        ),
        "note": ParagraphStyle(
            "Note",
            parent=base["BodyText"],
            fontName="Body",
            fontSize=9,
            leading=13,
            alignment=TA_JUSTIFY,
        ),
    }


# ---------------------------------------------------------------------------
# 1. multi_page_tables.pdf
# ---------------------------------------------------------------------------

REGIONS = [
    "EMEA",
    "North America",
    "LATAM",
    "APAC",
    "Nordics",
    "DACH",
    "Benelux",
    "Iberia",
    "Oceania",
    "India",
]


def multi_page_tables(path: str) -> None:
    style = styles()
    story: list = [
        Paragraph("FY26 Revenue by Region", style["title"]),
        Paragraph(
            "The table below is one logical table split across three physical pages. "
            "The header row repeats on every page, the body rows alternate their fill, and "
            "each column carries its own alignment.",
            style["body"],
        ),
        Spacer(1, 12),
    ]

    header = ["Region", "Units sold", "Gross", "Net", "Growth"]
    data = [header]
    for page in range(3):
        for index, region in enumerate(REGIONS):
            units = 1200 + page * 37 + index * 11
            gross = units * (48 + index)
            net = int(gross * 0.82)
            growth = f"{(index + page) % 7 + 1}.{index}%"
            data.append(
                [
                    Paragraph(f"{region} {page + 1}", style["cell"]),
                    Paragraph(f"{units:,}", style["cell_right"]),
                    Paragraph(f"${gross:,}", style["cell_right"]),
                    Paragraph(f"${net:,}", style["cell_right"]),
                    Paragraph(growth, style["cell_center"]),
                ]
            )

    table = Table(
        data,
        colWidths=[1.55 * inch, 0.95 * inch, 1.0 * inch, 1.0 * inch, 0.8 * inch],
        repeatRows=1,
    )
    grid: list = [
        ("FONTNAME", (0, 0), (-1, 0), "Body-Bold"),
        ("TEXTCOLOR", (0, 0), (-1, 0), colors.white),
        ("BACKGROUND", (0, 0), (-1, 0), colors.HexColor("#2F3B4C")),
        ("GRID", (0, 0), (-1, -1), 0.5, colors.HexColor("#8A94A6")),
        ("ALIGN", (1, 1), (1, -1), "RIGHT"),
        ("ALIGN", (2, 1), (3, -1), "RIGHT"),
        ("ALIGN", (4, 1), (4, -1), "CENTER"),
        ("VALIGN", (0, 0), (-1, -1), "MIDDLE"),
        ("LEFTPADDING", (0, 0), (-1, -1), 6),
        ("RIGHTPADDING", (0, 0), (-1, -1), 6),
    ]
    for row in range(1, len(data)):
        if row % 2 == 0:
            grid.append(("BACKGROUND", (0, row), (-1, row), colors.HexColor("#EDF1F7")))
    table.setStyle(TableStyle(grid))
    story.append(table)

    story.append(PageBreak())
    story.append(Paragraph("Year-over-year roll-up", style["h1"]))
    story.append(
        Paragraph(
            "Merged spans are a separate table so a reader that ignores spans cannot hide it "
            "behind the first grid.",
            style["body"],
        )
    )
    story.append(Spacer(1, 10))

    span_header = [
        "Quarter",
        "Marketing",
        "Research",
        "Support",
        "Total",
    ]
    span_rows = [
        [
            Paragraph("Q1", style["cell"]),
            Paragraph("120", style["cell_right"]),
            Paragraph("240", style["cell_right"]),
            Paragraph("80", style["cell_right"]),
            Paragraph("440", style["cell_right"]),
        ],
        [
            Paragraph("Q2", style["cell"]),
            Paragraph("138", style["cell_right"]),
            Paragraph("252", style["cell_right"]),
            Paragraph("91", style["cell_right"]),
            Paragraph("481", style["cell_right"]),
        ],
        [
            Paragraph("H1", style["cell_center"]),
            Paragraph("258", style["cell_right"]),
            Paragraph("492", style["cell_right"]),
            Paragraph("171", style["cell_right"]),
            Paragraph("921", style["cell_right"]),
        ],
    ]
    span_table = Table(
        [[Paragraph(cell, style["cell"]) for cell in span_header]] + span_rows,
        colWidths=[0.9 * inch, 1.1 * inch, 1.1 * inch, 1.0 * inch, 1.0 * inch],
    )
    span_table.setStyle(
        TableStyle(
            [
                ("FONTNAME", (0, 0), (-1, 0), "Body-Bold"),
                ("BACKGROUND", (0, 0), (-1, 0), colors.HexColor("#2F3B4C")),
                ("TEXTCOLOR", (0, 0), (-1, 0), colors.white),
                ("GRID", (0, 0), (-1, -1), 0.5, colors.HexColor("#8A94A6")),
                ("SPAN", (0, 1), (0, 2)),
                ("SPAN", (1, 3), (3, 3)),
            ]
        )
    )
    story.append(span_table)
    story.append(Spacer(1, 10))
    story.append(
        Paragraph(
            "H1 collapses the two quarters into a single row; the marketing, research and "
            "support columns are merged on that row.",
            style["note"],
        )
    )
    build(path, story)


# ---------------------------------------------------------------------------
# 2. rich_media_layout.pdf
# ---------------------------------------------------------------------------


def gradient_png(width: int, height: int, mode: str = "RGB") -> io.BytesIO:
    image = Image.new(mode, (width, height), (255, 255, 255))
    draw = ImageDraw.Draw(image)
    for x in range(width):
        shade = int(40 + 200 * x / max(1, width - 1))
        draw.line([(x, 0), (x, height)], fill=(shade, (shade * 2) % 256, 255 - shade))
    for y in range(0, height, max(1, height // 8)):
        draw.line([(0, y), (width, y)], fill=(255, 255, 255))
    buffer = io.BytesIO()
    image.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def gradient_jpeg(width: int, height: int) -> io.BytesIO:
    image = Image.new("RGB", (width, height), (250, 250, 250))
    draw = ImageDraw.Draw(image)
    for y in range(height):
        shade = int(30 + 180 * y / max(1, height - 1))
        draw.line([(0, y), (width, y)], fill=(250 - shade, shade // 2, shade))
    for x in range(0, width, max(1, width // 10)):
        draw.line([(x, 0), (x, height)], fill=(255, 200, 120))
    buffer = io.BytesIO()
    image.save(buffer, format="JPEG", quality=85)
    buffer.seek(0)
    return buffer


def rich_media_layout(path: str) -> None:
    style = styles()

    def page(canvas, doc):
        width, height = LETTER
        canvas.saveState()
        # Stroked vector rules: page frame, a horizontal rule and a vertical divider.
        canvas.setLineWidth(0.8)
        canvas.setStrokeColor(colors.HexColor("#4A5568"))
        canvas.rect(0.75 * inch, 0.75 * inch, width - 1.5 * inch, height - 1.5 * inch)
        canvas.setLineWidth(1.4)
        canvas.line(0.75 * inch, height - 1.15 * inch, width - 0.75 * inch, height - 1.15 * inch)
        canvas.line(width / 2, height - 1.2 * inch, width / 2, height - 2.4 * inch)
        # Tinted callout box with a border.
        canvas.setFillColor(colors.HexColor("#FFF6DA"))
        canvas.setStrokeColor(colors.HexColor("#D9A400"))
        canvas.roundRect(
            0.9 * inch, height - 4.4 * inch, width - 1.8 * inch, 0.9 * inch, 6, stroke=1, fill=1
        )
        canvas.setFillColor(colors.black)
        canvas.setFont("Body", 9)
        canvas.drawString(1.05 * inch, height - 3.75 * inch, "Callout: retention lifted 4.2 points")
        canvas.drawString(
            1.05 * inch,
            height - 3.95 * inch,
            "This text sits inside a filled, stroked background box.",
        )
        # Second, narrower callout so the analyser sees a small box and a wide one.
        canvas.setFillColor(colors.HexColor("#E8F1FF"))
        canvas.setStrokeColor(colors.HexColor("#3B82F6"))
        canvas.roundRect(
            0.9 * inch, height - 5.4 * inch, 2.6 * inch, 0.7 * inch, 4, stroke=1, fill=1
        )
        canvas.setFont("Body", 8)
        canvas.drawString(1.0 * inch, height - 5.15 * inch, "Inline figures follow this box.")
        canvas.restoreState()

    doc = BaseDocTemplate(
        path,
        pagesize=LETTER,
        leftMargin=0.75 * inch,
        rightMargin=0.75 * inch,
        topMargin=0.9 * inch,
        bottomMargin=0.9 * inch,
        title="Rich media layout",
        author="llm-cleanup fixtures",
    )
    frame = Frame(
        doc.leftMargin,
        doc.bottomMargin,
        doc.width,
        doc.height,
        id="body",
        leftPadding=0,
        rightPadding=0,
        topPadding=6,
        bottomPadding=0,
    )
    doc.addPageTemplates([PageTemplate(id="all", frames=[frame], onPage=page)])

    png = gradient_png(320, 160)
    jpeg = gradient_jpeg(360, 200)
    inline_png = gradient_png(48, 24)

    story = [
        Paragraph("Rich media layout", style["title"]),
        Paragraph(
            "Figure 1 is a block PNG; figure 2 is a block JPEG. Both are drawn at their natural "
            "aspect ratio, so an extractor must report non-zero pixel dimensions for each.",
            style["body"],
        ),
        Spacer(1, 8),
        PlatypusImage(png, width=3.2 * inch, height=1.6 * inch),
        Spacer(1, 6),
        Paragraph("Figure 1 — block PNG placeholder.", style["note"]),
        Spacer(1, 12),
        PlatypusImage(jpeg, width=3.6 * inch, height=2.0 * inch),
        Spacer(1, 6),
        Paragraph("Figure 2 — block JPEG placeholder.", style["note"]),
        Spacer(1, 12),
        Paragraph(
            "An inline icon follows in the running text so the reader sees an image between two "
            "sentences rather than between blocks.",
            style["body"],
        ),
        Spacer(1, 4),
        PlatypusImage(inline_png, width=0.6 * inch, height=0.3 * inch),
        Spacer(1, 4),
        Paragraph("The icon above is 48 by 24 pixels and inline-sized.", style["note"]),
        Spacer(1, 10),
        Paragraph("Media inventory", style["h2"]),
        Paragraph(
            "One PNG, one JPEG and one inline PNG are embedded; the vector layer adds a page "
            "frame, a horizontal rule, a vertical divider and two tinted boxes.",
            style["body"],
        ),
    ]
    doc.build(story)


# ---------------------------------------------------------------------------
# 3. typographic_symbols.pdf
# ---------------------------------------------------------------------------

CODE_LINES = [
    "def summarize(values):",
    "    total = sum(values)",
    "    if total == 0:",
    "        return None",
    "    return total / len(values)  # mean",
]


def typographic_symbols(path: str) -> None:
    style = styles()
    story = [
        Paragraph("Typographic coverage", style["title"]),
        Paragraph(
            "Greek: \u03b1 \u03b2 \u03b3 \u0394 \u03a9 \u03c0 \u03bc \u03c3. Extended Latin: "
            "\u00e9 \u00f1 \u00fc \u00df \u00e6 \u0153 \u0141 \u017d. Arrows: \u2192 \u2190 "
            "\u2194 \u21d2 \u2190 \u21c4 \u25b6.",
            style["body"],
        ),
        Paragraph(
            "Mathematics: \u2211 \u222b \u221a \u2248 \u2260 \u2264 \u2265 \u00b1 \u00d7 "
            "\u00f7 \u221e \u2202 \u2207 \u2208 \u2229 \u222a \u2282 \u2286.",
            style["body"],
        ),
        Paragraph(
            "Currency and punctuation: \u20ac \u00a3 \u00a5 \u20b9 \u20bd \u2014 \u2013 "
            "\u2026 \u201cquoted\u201d \u2018single\u2019 \u00bfd\u00f3nde? \u00a1hola!",
            style["body"],
        ),
        Paragraph("Indented code", style["h2"]),
        Paragraph(
            "The block below is indented four spaces and uses a monospaced face; its leading "
            "whitespace is structural and must survive conversion.",
            style["body"],
        ),
        Spacer(1, 6),
    ]
    code_style = ParagraphStyle(
        "Code",
        parent=style["body"],
        fontName="Mono",
        fontSize=8.5,
        leading=11,
        leftIndent=18,
        backColor=colors.HexColor("#F4F6F8"),
        borderColor=colors.HexColor("#CBD3DD"),
        borderWidth=0.6,
        borderPadding=6,
    )
    for line in CODE_LINES:
        story.append(Paragraph(line.replace(" ", "&nbsp;"), code_style))
    story.append(Spacer(1, 10))
    story.append(
        Paragraph(
            "Ligature-adjacent pairs: fi fl ffi ffl, and combining marks: e\u0301 a\u0308 "
            "n\u0303, so a decoder that drops non-ASCII bytes is obvious.",
            style["body"],
        )
    )
    story.append(
        Paragraph(
            "Zero-width probe: a\u200bb\u200cc\ufeffd (Word joiner, ZWNJ, ZWSP, BOM) \u2014 "
            "these are exactly what the sanitizer removes at Light.",
            style["note"],
        )
    )
    build(path, story)


# ---------------------------------------------------------------------------
# 4. mixed_columns.pdf
# ---------------------------------------------------------------------------

LATIN = (
    "Revenue growth accelerated in every region while support costs stayed flat, which is the "
    "single most useful sentence in the whole report for anyone skimming it. "
)


def wrapped(text: str, width: int) -> list[str]:
    return textwrap.wrap(text, width=width, break_long_words=False, break_on_hyphens=False)


def column_block(
    canvas, x: float, top: float, width_chars: int, paragraphs: int, label: str
) -> float:
    """Draw wrapped body text; returns the y coordinate after the last line."""
    canvas.setFont("Body", 9.5)
    y = top
    for index in range(paragraphs):
        text = f"{label} paragraph {index + 1}. " + LATIN * 2
        for line in wrapped(text, width_chars):
            canvas.drawString(x, y, line)
            y -= 12.5
        y -= 6
    return y


def mixed_columns(path: str) -> None:
    from reportlab.pdfgen import canvas as canvas_module

    canvas = canvas_module.Canvas(path, pagesize=LETTER, invariant=1)
    canvas.setTitle("Mixed columns")
    width, height = LETTER
    margin = 0.75 * inch

    # Page 1 — single-column banner.
    canvas.setFont("Body-Bold", 26)
    canvas.drawString(margin, height - 1.4 * inch, "Annual Review")
    canvas.setFont("Body", 11)
    banner = (
        "This page is a single full-width banner. The body text spans the whole content width, "
        "so a reader that assumes two columns would interleave sentences that do not belong "
        "together. " * 2
    )
    y = height - 1.9 * inch
    canvas.setFont("Body", 10)
    for line in wrapped(banner, 92):
        canvas.drawString(margin, y, line)
        y -= 14
    canvas.showPage()

    # Page 2 — two columns.
    canvas.setFont("Body-Bold", 16)
    canvas.drawString(margin, height - 1.2 * inch, "Two-column body")
    top = height - 1.7 * inch
    column_block(canvas, margin, top, 42, 3, "Left column")
    column_block(canvas, margin + 3.4 * inch, top, 42, 3, "Right column")
    canvas.showPage()

    # Page 3 — three columns.
    canvas.setFont("Body-Bold", 16)
    canvas.drawString(margin, height - 1.2 * inch, "Three-column body")
    top = height - 1.7 * inch
    column_block(canvas, margin, top, 26, 3, "First column")
    column_block(canvas, margin + 2.3 * inch, top, 26, 3, "Second column")
    column_block(canvas, margin + 4.6 * inch, top, 26, 3, "Third column")
    canvas.showPage()

    # Page 4 — single-column endnotes.
    canvas.setFont("Body-Bold", 14)
    canvas.drawString(margin, height - 1.2 * inch, "Endnotes")
    canvas.setFont("Body", 9)
    y = height - 1.6 * inch
    for index in range(1, 9):
        note = wrapped(f"{index}. Endnote {index}. " + LATIN, 88)
        for line in note:
            canvas.drawString(margin, y, line)
            y -= 12
        y -= 4
    canvas.showPage()
    canvas.save()


# ---------------------------------------------------------------------------
# 5. ai_slop_samples.pdf
# ---------------------------------------------------------------------------


def ai_slop_samples(path: str) -> None:
    style = styles()
    story = [
        Paragraph("Certainly! Here is your summary", style["title"]),
        Paragraph(
            "As an AI language model, I cannot provide legal advice, and this document is for "
            "informational purposes only.",
            style["body"],
        ),
        Paragraph(
            "Sure! Below you will find a concise overview of the pilot programme, prepared in "
            "the format you requested.",
            style["body"],
        ),
        Paragraph("Pilot programme results", style["h1"]),
        Paragraph(
            "The six-week pilot ran in three offices. Support tickets per thousand orders fell "
            "from 41 to 26, and the median resolution time fell from nine hours to four. "
            "Two of the three offices asked to keep the new workflow.",
            style["body"],
        ),
        Paragraph(
            "Please note that this is a hypothetical scenario and the numbers may not reflect "
            "your environment.",
            style["body"],
        ),
        Paragraph("Costs", style["h2"]),
        Paragraph(
            "Licensing cost 14,000 euros for the year; training cost 3,200 euros one-off. The "
            "team recovered the licensing cost by week nine.",
            style["body"],
        ),
        Paragraph(
            "I hope this helps! Let me know if you'd like me to expand on any section, and feel "
            "free to ask if you have any questions.",
            style["body"],
        ),
        Paragraph(
            "This response was generated by an AI assistant and may contain mistakes.",
            style["body"],
        ),
    ]
    build(path, story)


# ---------------------------------------------------------------------------
# Shared builder
# ---------------------------------------------------------------------------


def build(path: str, story: list) -> None:
    doc = BaseDocTemplate(
        path,
        pagesize=A4,
        leftMargin=0.9 * inch,
        rightMargin=0.9 * inch,
        topMargin=0.9 * inch,
        bottomMargin=0.9 * inch,
        title=os.path.basename(path),
        author="llm-cleanup fixtures",
    )
    frame = Frame(doc.leftMargin, doc.bottomMargin, doc.width, doc.height, id="body")
    doc.addPageTemplates([PageTemplate(id="body", frames=[frame])])
    doc.build(story)


def main() -> None:
    register_fonts()
    os.makedirs(FIXTURES, exist_ok=True)
    targets = {
        "multi_page_tables.pdf": multi_page_tables,
        "rich_media_layout.pdf": rich_media_layout,
        "typographic_symbols.pdf": typographic_symbols,
        "mixed_columns.pdf": mixed_columns,
        "ai_slop_samples.pdf": ai_slop_samples,
    }
    for name, builder in targets.items():
        path = os.path.join(FIXTURES, name)
        builder(path)
        print(f"wrote {os.path.relpath(path, HERE)} ({os.path.getsize(path):,} bytes)")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Generate the reproducible PDF corpus used by the document-IR integration tests.

    python3 -m pip install reportlab pillow
    python3 scripts/generate_fixtures.py            # write tests/fixtures/*.pdf + manifest.json
    python3 scripts/generate_fixtures.py --verify  # prove a rerun is byte-identical

The generated PDFs are committed: `cargo test` never needs Python, network access or a font
server. `rl_config.invariant` plus fixed Info-dictionary values makes every run byte-identical.

Ground truth for the Rust assertions lives in `tests/fixtures/manifest.json`; the generator is
the single source of truth for both the PDFs and that manifest.

Fixtures
--------
kerning_and_typography.pdf   Varied font families, italic/bold/bold-italic, extreme character
                             tracking/kerning via Tj displacements and Tc/Tw, mixed font sizes on
                             identical baselines, monospace code blocks, and natural word gaps
                             that must NOT become spaced-out characters.
multi_page_tables.pdf        Multi-page tables with explicit borders, rowspans, colspans,
                             alternating fills, right-aligned numeric data, and headers repeating
                             across page breaks.
rich_media_and_symbols.pdf   Inline raster images, vector rules, mathematical notation
                             (sum, integral, not-equal, approx), currency symbols, and extended
                             Unicode.
multi_column_newsletter.pdf  2-column and 3-column articles with headers spanning all columns,
                             callouts, and footnotes.
ai_slop_samples.pdf          Boilerplate AI disclaimers, conversational markers, and repetitive
                             summaries embedded in complex layouts.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import sys
import textwrap

from PIL import Image, ImageDraw
from reportlab import rl_config

# ReportLab emits a creation date and a random document ID unless invariant mode is on. Both
# would make a regenerated fixture differ byte-for-byte from the committed one.
rl_config.invariant = 1

from reportlab.lib import colors  # noqa: E402
from reportlab.lib.enums import TA_CENTER, TA_JUSTIFY, TA_LEFT, TA_RIGHT  # noqa: E402
from reportlab.lib.pagesizes import A4, LETTER  # noqa: E402
from reportlab.lib.styles import ParagraphStyle, getSampleStyleSheet  # noqa: E402
from reportlab.lib.utils import ImageReader  # noqa: E402
from reportlab.lib.units import inch  # noqa: E402
from reportlab.pdfbase import pdfmetrics  # noqa: E402
from reportlab.pdfbase.ttfonts import TTFont  # noqa: E402
from reportlab.pdfgen import canvas as canvas_module  # noqa: E402
from reportlab.platypus import (  # noqa: E402
    BaseDocTemplate,
    Flowable,
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
FIXTURES = os.path.join(HERE, "tests", "fixtures")

FONT_DIR = "/usr/share/fonts/truetype/dejavu"
REGULAR = os.path.join(FONT_DIR, "DejaVuSans.ttf")
BOLD = os.path.join(FONT_DIR, "DejaVuSans-Bold.ttf")
ITALIC = os.path.join(FONT_DIR, "DejaVuSans-Oblique.ttf")
BOLDITALIC = os.path.join(FONT_DIR, "DejaVuSans-BoldOblique.ttf")
MONO = os.path.join(FONT_DIR, "DejaVuSansMono.ttf")
MONO_BOLD = os.path.join(FONT_DIR, "DejaVuSansMono-Bold.ttf")

# Try alternative italic names
for alt in ["/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Italic.ttf"]:
    if os.path.exists(alt) and not os.path.exists(ITALIC):
        ITALIC = alt

# Fixed sentinel vocabulary. The Rust tests read the same strings from manifest.json.
SYMBOLS = [
    "α", "β", "γ", "Δ", "Ω", "π", "μ", "σ",          # Greek
    "Ж", "й", "щ", "Ё",                                # Cyrillic
    "é", "ñ", "ü", "ß", "æ", "œ", "Ł", "Ž", "Å",      # extended Latin
    "∑", "∫", "√", "≈", "≠", "≤", "≥", "±", "×", "÷",  # mathematics
    "∞", "∂", "∇", "∈", "∩", "∪", "⊂", "⊆",
    "→", "←", "↔", "⇒", "⇔", "⇄", "▶",                # arrows
    "€", "£", "¥", "₹", "₽", "—", "–", "…",           # currency + punctuation
    "“quoted”", "‘single’", "¿dónde?", "¡hola!",
    "e\u0301", "a\u0308", "n\u0303",                    # combining marks
]

REGIONS = [
    "EMEA", "North America", "LATAM", "APAC", "Nordics",
    "DACH", "Benelux", "Iberia", "Oceania", "India",
]
PRIMARY_ROWS = 60  # three pages of 20 rows; the header repeats on each page

COLUMN_MARKERS = [
    "BANNER-ONE",
    "COL2-LEFT",
    "COL2-RIGHT",
    "COL3-FIRST",
    "COL3-SECOND",
    "COL3-THIRD",
    "BANNER-TWO",
    "COL2-REPEAT-LEFT",
    "COL2-REPEAT-RIGHT",
    "ENDNOTES",
]

SLOP_KEEP = [
    "Pilot programme results",
    "The six-week pilot ran in three offices",
    "Support tickets per thousand orders fell from 41 to 26",
    "Licensing cost 14,000 euros",
    "The team recovered the licensing cost by week nine",
    "Costs",
    "Risk register",
    "The risk register is reviewed every fortnight",
    "def summarize(values):",
]

SLOP_DROP = [
    "As an AI language model",
    "Certainly!",
    "I hope this helps",
    "feel free to ask if you have any questions",
    "This response was generated by",
    "for informational purposes only",
    "[/INST]",
    "<p>",
    "[END]",
    "in conclusion in conclusion in conclusion",
]

# Kerning fixture vocabulary
KERNING_SENTINELS = [
    "The quick brown fox jumps over the lazy dog",
    "Kerning test: AVATAR WAVE To",
    "Typography: fi fl ff ffi ffl ligatures",
    "Mixed sizes on same baseline",
    "Natural word spacing must survive",
]


def register_fonts() -> None:
    """DejaVu covers Greek, Cyrillic, math and arrows without glyph substitution."""
    fonts_to_register = [
        ("Body", REGULAR),
        ("Body-Bold", BOLD),
        ("Mono", MONO),
    ]
    # Optional italic variants
    if os.path.exists(BOLD):
        fonts_to_register.append(("Body-Bold", BOLD))
    if os.path.exists(MONO_BOLD):
        fonts_to_register.append(("Mono-Bold", MONO_BOLD))

    for name, path in fonts_to_register:
        if not os.path.exists(path):
            raise SystemExit(f"missing font: {path} (install fonts-dejavu-core)")
        try:
            pdfmetrics.registerFont(TTFont(name, path))
        except Exception:
            pass  # already registered

    # Try italic variants if available
    for name, path in [("Body-Italic", ITALIC), ("Body-BoldItalic", BOLDITALIC)]:
        if os.path.exists(path):
            try:
                pdfmetrics.registerFont(TTFont(name, path))
            except Exception:
                pass
        else:
            # Fallback: map italic to regular
            try:
                pdfmetrics.registerFont(TTFont(name, REGULAR))
            except Exception:
                pass

    try:
        pdfmetrics.registerFontFamily(
            "Body", normal="Body", bold="Body-Bold", italic="Body-Italic", boldItalic="Body-BoldItalic"
        )
    except Exception:
        pass


def styles() -> dict[str, ParagraphStyle]:
    base = getSampleStyleSheet()
    return {
        "title": ParagraphStyle(
            "Title", parent=base["Title"], fontName="Body-Bold", fontSize=23,
            leading=28, spaceAfter=12,
        ),
        "h1": ParagraphStyle(
            "H1", parent=base["Heading1"], fontName="Body-Bold", fontSize=17, leading=22,
        ),
        "h2": ParagraphStyle(
            "H2", parent=base["Heading2"], fontName="Body-Bold", fontSize=13, leading=17,
        ),
        "h3": ParagraphStyle(
            "H3", parent=base["Heading3"], fontName="Body-Bold", fontSize=11, leading=14,
        ),
        "body": ParagraphStyle(
            "Body", parent=base["BodyText"], fontName="Body", fontSize=10, leading=14,
        ),
        "body_italic": ParagraphStyle(
            "BodyItalic", parent=base["BodyText"], fontName="Body-Italic", fontSize=10, leading=14,
        ),
        "body_bold": ParagraphStyle(
            "BodyBold", parent=base["BodyText"], fontName="Body-Bold", fontSize=10, leading=14,
        ),
        "body_bold_italic": ParagraphStyle(
            "BodyBoldItalic", parent=base["BodyText"], fontName="Body-BoldItalic", fontSize=10, leading=14,
        ),
        "small": ParagraphStyle(
            "Small", parent=base["BodyText"], fontName="Body", fontSize=8, leading=10,
        ),
        "large": ParagraphStyle(
            "Large", parent=base["BodyText"], fontName="Body", fontSize=14, leading=18,
        ),
        "cell": ParagraphStyle(
            "Cell", parent=base["BodyText"], fontName="Body", fontSize=8.5, leading=11,
        ),
        "cell_right": ParagraphStyle(
            "CellRight", parent=base["BodyText"], fontName="Body", fontSize=8.5,
            leading=11, alignment=TA_RIGHT,
        ),
        "cell_center": ParagraphStyle(
            "CellCenter", parent=base["BodyText"], fontName="Body", fontSize=8.5,
            leading=11, alignment=TA_CENTER,
        ),
        "note": ParagraphStyle(
            "Note", parent=base["BodyText"], fontName="Body", fontSize=9, leading=13,
            alignment=TA_JUSTIFY,
        ),
        "code": ParagraphStyle(
            "Code", parent=base["BodyText"], fontName="Mono", fontSize=8.5, leading=11,
            leftIndent=18, backColor=colors.HexColor("#F4F6F8"),
            borderColor=colors.HexColor("#CBD3DD"), borderWidth=0.6, borderPadding=6,
        ),
        "pre": ParagraphStyle(
            "CodePre", fontName="Mono", fontSize=8.5, leading=11, leftIndent=18,
            backColor=colors.HexColor("#F4F6F8"),
            borderColor=colors.HexColor("#CBD3DD"), borderWidth=0.6, borderPadding=6,
            spaceBefore=6, spaceAfter=10,
        ),
        "mono_inline": ParagraphStyle(
            "MonoInline", fontName="Mono", fontSize=9, leading=12,
        ),
    }


# ---------------------------------------------------------------------------
# Shared builders / page furniture
# ---------------------------------------------------------------------------

RUNNING_HEADER = "QUARTERLY REVENUE \u2014 INTERNAL"
RUNNING_FOOTER = "llm-cleanup fixture \u2022 confidential"


def furniture(canvas, doc, *, rules: bool = False, cards: bool = False) -> None:
    """Paint a repeated header/footer band, and optionally vector rules / tinted cards."""
    width, height = doc.pagesize
    canvas.saveState()
    canvas.setFont("Body-Bold", 8)
    canvas.setFillColor(colors.HexColor("#5A6675"))
    canvas.drawString(0.7 * inch, height - 0.42 * inch, RUNNING_HEADER)
    canvas.setFont("Body", 8)
    canvas.drawRightString(width - 0.7 * inch, 0.42 * inch, f"{RUNNING_FOOTER} \u2022 page {doc.page}")

    if rules:
        canvas.setLineWidth(0.8)
        canvas.setStrokeColor(colors.HexColor("#4A5568"))
        canvas.rect(0.7 * inch, 0.7 * inch, width - 1.4 * inch, height - 1.4 * inch)
        canvas.setLineWidth(1.4)
        canvas.line(0.7 * inch, height - 1.15 * inch, width - 0.7 * inch, height - 1.15 * inch)
        canvas.line(width / 2, height - 1.2 * inch, width / 2, height - 2.4 * inch)
        canvas.setLineWidth(1.0)
        canvas.setStrokeColor(colors.HexColor("#D9A400"))
        for index in range(6):
            x = 0.95 * inch + index * 0.62 * inch
            canvas.line(x, height - 1.35 * inch, x + 0.42 * inch, height - 1.35 * inch)

    if cards:
        canvas.setFillColor(colors.HexColor("#FFF6DA"))
        canvas.setStrokeColor(colors.HexColor("#D9A400"))
        canvas.roundRect(
            0.9 * inch, height - 4.4 * inch, width - 1.8 * inch, 0.9 * inch, 6,
            stroke=1, fill=1,
        )
        canvas.setFillColor(colors.black)
        canvas.setFont("Body", 9)
        canvas.drawString(1.05 * inch, height - 3.75 * inch, "Callout: retention lifted 4.2 points")
        canvas.drawString(
            1.05 * inch, height - 3.95 * inch,
            "This text sits inside a filled, stroked background box.",
        )
        canvas.setFillColor(colors.HexColor("#E8F1FF"))
        canvas.setStrokeColor(colors.HexColor("#3B82F6"))
        canvas.roundRect(
            0.9 * inch, height - 5.4 * inch, 2.6 * inch, 0.7 * inch, 4,
            stroke=1, fill=1,
        )
        canvas.setFillColor(colors.black)
        canvas.setFont("Body", 8)
        canvas.drawString(1.0 * inch, height - 5.15 * inch, "Inline figures follow this box.")
    canvas.restoreState()


def build(path: str, story: list, *, pagesize=A4, margin=0.75 * inch, on_page=None) -> None:
    doc = BaseDocTemplate(
        path,
        pagesize=pagesize,
        leftMargin=margin,
        rightMargin=margin,
        topMargin=0.85 * inch,
        bottomMargin=0.8 * inch,
        title=os.path.basename(path),
        author="llm-cleanup fixture generator",
        subject="deterministic document-IR regression corpus",
        creator="scripts/generate_fixtures.py",
        invariant=1,
    )
    frame = Frame(doc.leftMargin, doc.bottomMargin, doc.width, doc.height, id="body")
    doc.addPageTemplates([PageTemplate(id="body", frames=[frame], onPage=on_page or furniture)])
    doc.build(story)


# ---------------------------------------------------------------------------
# 1. kerning_and_typography.pdf  (NEW - Priority 1)
# ---------------------------------------------------------------------------

def kerning_and_typography(path: str) -> None:
    """Exercise glyph clustering: extreme tracking, mixed fonts/sizes on one baseline."""
    s = styles()
    story: list = [
        Paragraph("Kerning &amp; Typography Stress Test", s["title"]),
        Paragraph(
            "This fixture validates that glyph clustering and word reconstruction handle "
            "extreme tracking, mixed font sizes on identical baselines, and varied weights "
            "without collapsing words into spaced-out characters.",
            s["body"],
        ),
        Spacer(1, 12),
        Paragraph("Font Family &amp; Weight Variations", s["h1"]),
        Paragraph("Regular weight body text in the default typeface.", s["body"]),
        Paragraph("Bold weight body text for emphasis and headings.", s["body_bold"]),
        Paragraph("Italic weight for citations and foreign phrases.", s["body_italic"]),
        Paragraph("Bold-italic weight for maximum emphasis.", s["body_bold_italic"]),
        Spacer(1, 8),
    ]

    # Mixed font sizes on identical baseline - critical for y-tolerance clustering
    story.append(Paragraph("Mixed Font Sizes on Shared Baseline", s["h2"]))
    story.append(Paragraph(
        'Text with <font size="8">small 8pt</font> and '
        '<font size="14">large 14pt</font> and '
        '<font size="10">normal 10pt</font> sharing one baseline tests '
        "y-coordinate grouping.",
        s["body"],
    ))
    story.append(Spacer(1, 6))
    story.append(Paragraph(
        'Another mixed line: <font size="6">tiny</font> '
        '<font size="18">HUGE</font> '
        '<font size="10">normal</font> all on the same line.',
        s["body"],
    ))

    # Monospace code blocks
    story.append(Spacer(1, 12))
    story.append(Paragraph("Monospace Code Blocks", s["h1"]))
    story.append(Paragraph(
        "The following code uses a monospace face and must keep indentation intact:",
        s["body"],
    ))

    # Manual code block with monospace - we'll use canvas drawing for raw Tj testing
    story.append(Spacer(1, 6))

    # Use a table to show monospace content without Platypus reflow
    code_lines = [
        "def fibonacci(n):",
        "    if n <= 1:",
        "        return n",
        "    return fibonacci(n-1) + fibonacci(n-2)",
        "",
        "result = fibonacci(10)  # Should be 55",
        "print(f\"fib(10) = {result}\")",
    ]
    for line in code_lines:
        # Use preformatted with monospace
        safe = line.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
        # Use font tag for monospace
        story.append(Paragraph(f'<font face="Mono" size="8">{safe}</font>', s["mono_inline"]))

    story.append(Spacer(1, 8))
    story.append(Paragraph(
        "Code above must not be rewritten; it contains intentional formatting tokens.",
        s["note"],
    ))

    # Extreme tracking/kerning - drawn via canvas for low-level Tj control
    story.append(Spacer(1, 12))
    story.append(Paragraph("Natural Word Spacing", s["h1"]))
    story.append(Paragraph(
        "The quick brown fox jumps over the lazy dog. This sentence tests natural "
        "word spacing: gaps between words should be spaces, not character splits.",
        s["body"],
    ))
    story.append(Paragraph(
        "Pack my box with five dozen liquor jugs. Another pangram for spacing validation.",
        s["body"],
    ))
    story.append(Paragraph(
        "Typography ligatures: fi fl ffi ffl should remain as combined glyphs or separate "
        "characters without extra spacing artifacts.",
        s["body"],
    ))

    # Color typography
    story.append(Spacer(1, 12))
    story.append(Paragraph("Colored Typography", s["h2"]))
    story.append(Paragraph(
        'Color test: <font color="#C53030">red text \u03b1 \u2192 \u03a9</font> and '
        '<font color="#2B6CB0">blue text \u2211 \u2260 \u00b1</font> must keep RGB values. '
        'Colors are structural, not content.',
        s["body"],
    ))

    # Extreme tracking via low-level canvas - we create a page with custom Tj operations
    story.append(PageBreak())
    story.append(Paragraph("Low-Level Kerning Validation", s["h1"]))
    story.append(Paragraph(
        "The following lines are drawn with precise glyph positioning to test "
        "kerning extraction. Each line's words must reconstruct correctly.",
        s["body"],
    ))
    story.append(Spacer(1, 8))

    # These will be validated: words should NOT appear as "t h i s   i s"
    for sentinel in KERNING_SENTINELS:
        safe = sentinel.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
        story.append(Paragraph(safe, s["body"]))
        story.append(Spacer(1, 4))

    story.append(Spacer(1, 8))
    story.append(Paragraph("End of Kerning Fixture", s["h2"]))
    story.append(Paragraph(
        "If any exported text contains spaced-out characters like \"t h i s\", "
        "the extractor's glyph clustering is broken.",
        s["note"],
    ))

    # Add a canvas-drawn page with extreme tracking via manual Tj positioning
    def kerning_page(canvas, doc):
        furniture(canvas, doc)
        if doc.page == 3:
            # Draw text with varying character spacing on same baseline
            canvas.saveState()
            y = doc.pagesize[1] - 2.5 * inch
            # Normal spacing baseline
            canvas.setFont("Body", 10)
            canvas.drawString(0.75 * inch, y, "Normal spacing: Hello World Test")
            y -= 16
            # Tight tracking - characters close together
            canvas.setFont("Body", 10)
            # Use charSpace to tighten
            canvas.setCharSpace(-0.5)
            canvas.drawString(0.75 * inch, y, "Tight tracking: Hello World Test")
            canvas.setCharSpace(0)
            y -= 16
            # Wide tracking
            canvas.setCharSpace(1.5)
            canvas.drawString(0.75 * inch, y, "Wide tracking: Hello World Test")
            canvas.setCharSpace(0)
            y -= 16
            # Mixed sizes on same Y - the critical test
            canvas.setFont("Body", 8)
            canvas.drawString(0.75 * inch, y, "Small ")
            w1 = pdfmetrics.stringWidth("Small ", "Body", 8)
            canvas.setFont("Body", 14)
            canvas.drawString(0.75 * inch + w1, y, "LARGE ")
            w2 = pdfmetrics.stringWidth("LARGE ", "Body", 14)
            canvas.setFont("Body", 10)
            canvas.drawString(0.75 * inch + w1 + w2, y, "normal all same baseline")
            y -= 20
            # Kerning pairs - AV, To, Wa etc that have negative kerning
            canvas.setFont("Body", 14)
            canvas.drawString(0.75 * inch, y, "AVATAR WAVE To Va We kerning pairs")
            y -= 16
            canvas.setFont("Body", 10)
            canvas.drawString(0.75 * inch, y, "The end of manual kerning tests.")
            canvas.restoreState()

    # Build with custom page handler for last page
    doc = BaseDocTemplate(
        path,
        pagesize=A4,
        leftMargin=0.75 * inch,
        rightMargin=0.75 * inch,
        topMargin=0.85 * inch,
        bottomMargin=0.8 * inch,
        title=os.path.basename(path),
        author="llm-cleanup fixture generator",
        subject="deterministic document-IR regression corpus",
        creator="scripts/generate_fixtures.py",
        invariant=1,
    )
    frame = Frame(doc.leftMargin, doc.bottomMargin, doc.width, doc.height, id="body")
    doc.addPageTemplates([PageTemplate(id="body", frames=[frame], onPage=kerning_page)])
    doc.build(story)


# ---------------------------------------------------------------------------
# 2. multi_page_tables.pdf  (NEW - replaces complex_tables semantic but more explicit)
# ---------------------------------------------------------------------------

def multi_page_tables(path: str) -> None:
    s = styles()
    story: list = [
        Paragraph("FY26 Revenue by Region \u2014 Multi-Page Tables", s["title"]),
        Paragraph(
            "One logical table spans three physical pages. The header repeats on every page, "
            "body rows alternate their fill, and each numeric column has its own alignment. "
            "The running header and footer are page furniture, not document content.",
            s["body"],
        ),
        Spacer(1, 10),
    ]

    header = ["Region", "Units sold", "Gross", "Net", "Growth"]
    data = [header]
    for index in range(PRIMARY_ROWS):
        region = REGIONS[index % len(REGIONS)]
        units = 1_200 + index * 11
        gross = units * (48 + index % 7)
        net = int(gross * 0.82)
        growth = f"{(index % 7) + 1}.{index % 10}%"
        data.append([
            Paragraph(f"{region} {index + 1:02d}", s["cell"]),
            Paragraph(f"{units:,}", s["cell_right"]),
            Paragraph(f"${gross:,}", s["cell_right"]),
            Paragraph(f"${net:,}", s["cell_right"]),
            Paragraph(growth, s["cell_center"]),
        ])

    table = Table(
        data,
        colWidths=[1.55 * inch, 0.95 * inch, 1.0 * inch, 1.0 * inch, 0.8 * inch],
        repeatRows=1,
        rowHeights=[26] * len(data),
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
    story.append(Paragraph("Year-over-year roll-up", s["h1"]))
    story.append(Paragraph(
        "A second, independent grid exercises merged cells: one rowspan down the first "
        "column, one colspan across three numeric columns, and a multi-line cell.",
        s["body"],
    ))
    story.append(Spacer(1, 10))

    span_header = ["Quarter", "Marketing", "Research", "Support", "Total"]
    span_rows = [
        ["Q1", "120", "240", "80", "440"],
        ["Q2", "138", "252", "91", "481"],
        [
            Paragraph("H1<br/>combined", s["cell_center"]),
            "258", "492", "171",
            Paragraph("921<br/>verified", s["cell_right"]),
        ],
    ]
    span_table = Table(
        [[Paragraph(cell, s["cell"]) for cell in span_header]] + span_rows,
        colWidths=[0.9 * inch, 1.1 * inch, 1.1 * inch, 1.0 * inch, 1.0 * inch],
        rowHeights=[24, 24, 24, 32],
    )
    span_table.setStyle(TableStyle([
        ("FONTNAME", (0, 0), (-1, 0), "Body-Bold"),
        ("BACKGROUND", (0, 0), (-1, 0), colors.HexColor("#2F3B4C")),
        ("TEXTCOLOR", (0, 0), (-1, 0), colors.white),
        ("GRID", (0, 0), (-1, -1), 0.5, colors.HexColor("#8A94A6")),
        ("ALIGN", (1, 1), (-1, -1), "RIGHT"),
        ("VALIGN", (0, 0), (-1, -1), "MIDDLE"),
        ("SPAN", (0, 1), (0, 2)),       # rowspan
        ("SPAN", (1, 3), (3, 3)),       # colspan
    ]))
    story.append(span_table)
    story.append(Spacer(1, 10))
    story.append(Paragraph(
        "The H1 row is a single cell across the marketing, research and support columns. "
        "Its total is a multi-line cell, so GFM alone cannot represent it.",
        s["note"],
    ))

    # Also add a simple text paragraph to test plain text table export
    story.append(Spacer(1, 8))
    story.append(Paragraph("Table notes", s["h2"]))
    story.append(Paragraph(
        "All numeric columns in the primary table are right-aligned. "
        "The growth column is centered. These alignments must survive conversion.",
        s["body"],
    ))
    build(path, story)


# ---------------------------------------------------------------------------
# 3. rich_media_and_symbols.pdf  (NEW - combines rich_media + typographic_symbols)
# ---------------------------------------------------------------------------

def gradient_png(width: int, height: int, seed: int = 0) -> io.BytesIO:
    image = Image.new("RGB", (width, height), (255, 255, 255))
    draw = ImageDraw.Draw(image)
    for x in range(width):
        shade = int(40 + 190 * x / max(1, width - 1))
        draw.line([(x, 0), (x, height)], fill=(shade, (shade * 2 + seed) % 256, 255 - shade))
    for y in range(0, height, max(1, height // 10)):
        draw.line([(0, y), (width, y)], fill=(255, 255, 255))
    draw.rectangle([10, 10, width - 11, height - 11], outline=(20, 30, 40), width=3)
    buffer = io.BytesIO()
    image.save(buffer, format="PNG", optimize=False)
    buffer.seek(0)
    return buffer


def gradient_jpeg(width: int, height: int) -> io.BytesIO:
    image = Image.new("RGB", (width, height), (250, 250, 250))
    draw = ImageDraw.Draw(image)
    for y in range(height):
        shade = int(30 + 180 * y / max(1, height - 1))
        draw.line([(0, y), (width, y)], fill=(250 - shade, shade // 2, shade))
    for x in range(0, width, max(1, width // 12)):
        draw.line([(x, 0), (x, height)], fill=(255, 200, 120))
    buffer = io.BytesIO()
    image.save(buffer, format="JPEG", quality=88, optimize=False)
    buffer.seek(0)
    return buffer


class InlineIcon(Flowable):
    """A sentence with a 48x24 PNG drawn on the text baseline (a true inline image)."""

    def __init__(self, png: io.BytesIO, label: str):
        super().__init__()
        self.png = png.getvalue()
        self.label = label
        self.width = 6.4 * inch
        self.height = 0.42 * inch

    def draw(self):
        self.canv.setFont("Body", 9.5)
        self.canv.drawString(0, 6, self.label)
        label_width = pdfmetrics.stringWidth(self.label, "Body", 9.5)
        self.canv.drawImage(
            ImageReader(io.BytesIO(self.png)),
            label_width + 8, 1, width=0.6 * inch, height=0.3 * inch,
            mask="auto",
        )


CODE_LINES = [
    "def summarize(values):",
    "    total = sum(values)   # \u2211 over samples",
    "    if total == 0:",
    "        return None       # \u2205 result",
    "    return total / len(values)  # \u03bc \u00b1 \u03c3",
]


def rich_media_and_symbols(path: str) -> None:
    s = styles()
    png_large = gradient_png(900, 500)
    jpeg_large = gradient_jpeg(1200, 700)
    png_small = gradient_png(320, 180, seed=37)
    icon = gradient_png(48, 24, seed=11)

    wrapped = (
        "Text wraps beside this figure rather than jumping below it. A reader that only "
        "concatenates content-stream strings will still find the sentence, but a reader that "
        "tracks geometry must also keep the image placement and its measured DPI. " * 3
    )
    side_by_side = Table(
        [[
            PlatypusImage(png_small, width=1.5 * inch, height=0.84 * inch),
            Paragraph(wrapped, s["note"]),
        ]],
        colWidths=[1.7 * inch, 4.6 * inch],
    )
    side_by_side.setStyle(TableStyle([
        ("VALIGN", (0, 0), (-1, -1), "TOP"),
        ("LEFTPADDING", (0, 0), (-1, -1), 0),
        ("RIGHTPADDING", (0, 0), (-1, -1), 4),
    ]))

    # Symbol rows
    symbol_rows = [
        "Greek: \u03b1 \u03b2 \u03b3 \u0394 \u03a9 \u03c0 \u03bc \u03c3. Cyrillic: \u0416 \u0439 \u0449 \u0401. Extended Latin: \u00e9 \u00f1 \u00fc \u00df \u00e6 \u0153 \u0141 \u017d \u00c5.",
        "Mathematics: \u2211 \u222b \u221a \u2248 \u2260 \u2264 \u2265 \u00b1 \u00d7 \u00f7 \u221e \u2202 \u2207 \u2208 \u2229 \u222a \u2282 \u2286.",
        "Arrows: \u2192 \u2190 \u2194 \u21d2 \u21d4 \u21c4 \u25b6. Currency: \u20ac \u00a3 \u00a5 \u20b9 \u20bd. Punctuation: \u2014 \u2013 \u2026 \u201cquoted\u201d \u2018single\u2019.",
        "Spanish: \u00bfdonde? \u00a1hola! Ligature-adjacent pairs: fi fl ffi ffl.",
        "Combining marks: e\u0301 a\u0308 n\u0303 (a decoder that drops non-ASCII bytes is obvious).",
        "Zero-width probe: a\u200bb\u200cc\ufeffd (ZWSP, ZWNJ, BOM) \u2014 sanitizer fodder.",
    ]

    story: list = [
        Paragraph("Rich Media &amp; Symbols", s["title"]),
        Paragraph(
            "Two high-resolution raster assets and one inline icon are embedded verbatim. "
            "Vector rules, a dashed rule set and two tinted background cards are painted as "
            "paths and filled rectangles rather than as text. Mathematical notation and "
            "extended Unicode must survive intact.",
            s["body"],
        ),
        Spacer(1, 8),
        PlatypusImage(png_large, width=3.2 * inch, height=1.78 * inch),
        Paragraph("Figure 1 \u2014 900x500 PNG block image.", s["note"]),
        Spacer(1, 8),
        PlatypusImage(jpeg_large, width=3.6 * inch, height=2.1 * inch),
        Paragraph("Figure 2 \u2014 1200x700 JPEG block image.", s["note"]),
        Spacer(1, 8),
    ]

    # Symbol section
    story.append(Paragraph("Typographic &amp; Mathematical Symbols", s["h1"]))
    for row in symbol_rows:
        story.append(Paragraph(row, s["body"]))
    story.append(Paragraph(
        'Colour tag: <font color="#C53030">red \u03b1 \u2192 \u03a9</font> and '
        '<font color="#2B6CB0">blue \u2211 \u2260 \u00b1</font> must keep their source RGB values.',
        s["body"],
    ))

    # Code block with math comments
    story.append(Paragraph("Code with Mathematical Comments", s["h2"]))
    story.append(Paragraph(
        "The block below uses a monospaced face; its leading whitespace and Unicode comments "
        "are structural and must survive conversion intact.",
        s["body"],
    ))
    # Build preformatted code block
    from reportlab.platypus import Preformatted
    story.append(Preformatted("\n".join(CODE_LINES), s["pre"]))
    story.append(Paragraph(
        "Closing sentinel: \u03b1\u03b2\u03b3 \u2211\u222b\u2248\u2260\u00b1 \u2192 \u03a9 END-OF-SYMBOLS.",
        s["body"],
    ))

    # Page 2 with inline/wrapped media + vector rules
    story.append(PageBreak())
    story.append(Spacer(1, 4.95 * inch))
    story.append(Paragraph("Inline and wrapped media", s["h1"]))
    story.append(side_by_side)
    story.append(Spacer(1, 12))
    story.append(InlineIcon(icon, "An inline 48x24 icon follows this sentence:"))
    story.append(Paragraph("The icon above shares a baseline with the text, so it is an inline figure.", s["note"]))
    story.append(Spacer(1, 10))
    story.append(Paragraph("Vector layer", s["h2"]))
    story.append(Paragraph(
        "The page frame, header rule, vertical divider and dashed rule set are stroked "
        "vector paths. The yellow and blue cards are filled rounded rectangles with text "
        "on top; they must not leak into the prose as glyph soup.",
        s["body"],
    ))
    story.append(Paragraph("Currency symbols: \u20ac100 \u00a3500 \u00a51000 \u20b91000 \u20b95000 must all render.", s["body"]))
    story.append(Paragraph("Math: \u2211<sub>i=1</sub><sup>n</sup> x<sub>i</sub> \u2248 n\u00b7\u03bc \u00b1 3\u03c3, \u222b<sub>0</sub><sup>\u221e</sup> e<sup>-x</sup> dx = 1", s["body"]))

    def decorated_page(canv, doc):
        furniture(canv, doc, rules=(doc.page == 2), cards=(doc.page == 2))

    build(
        path, story, pagesize=LETTER, margin=0.75 * inch,
        on_page=decorated_page,
    )


# ---------------------------------------------------------------------------
# 4. multi_column_newsletter.pdf  (NEW - replaces multi_column_mixed)
# ---------------------------------------------------------------------------

LATIN = (
    "Revenue growth accelerated in every region while support costs stayed flat, which is the "
    "single most useful sentence in the whole report for anyone skimming it. "
)


def wrapped(text: str, width: int) -> list[str]:
    return textwrap.wrap(text, width=width, break_long_words=False, break_on_hyphens=False)


def column_block(
    canv, x: float, top: float, width_chars: int, paragraphs: int, label: str,
    *, footnote: bool = False,
) -> float:
    canv.setFont("Body", 9.5)
    y = top
    for index in range(paragraphs):
        text = f"{label} paragraph {index + 1}. {LATIN}"
        for line in wrapped(text, width_chars):
            canv.drawString(x, y, line)
            y -= 12.5
        if footnote:
            canv.setFont("Body", 7)
            canv.drawString(x + 6, y, f"[{label[:4]}-{index + 1}] See endnote {index + 1}.")
            canv.setFont("Body", 9.5)
            y -= 12.5
        y -= 6
    return y


def multi_column_newsletter(path: str) -> None:
    canv = canvas_module.Canvas(path, pagesize=LETTER, invariant=1)
    canv.setTitle("Newsletter - Multi-Column Layout")
    canv.setAuthor("llm-cleanup fixture generator")
    canv.setSubject("deterministic document-IR regression corpus")
    width, height = LETTER
    margin = 0.75 * inch

    # Page 1: full-width banner, then a two-column body with a tinted callout underneath.
    canv.setFont("Body-Bold", 25)
    canv.drawString(margin, height - 1.2 * inch, "BANNER-ONE Annual Review")
    canv.setFont("Body", 10)
    y = height - 1.65 * inch
    for line in wrapped(LATIN * 3, 92):
        canv.drawString(margin, y, line)
        y -= 14
    top = y - 18
    column_block(canv, margin, top, 42, 3, "COL2-LEFT")
    column_block(canv, margin + 3.4 * inch, top, 42, 3, "COL2-RIGHT")
    canv.setFillColor(colors.HexColor("#FFF6DA"))
    canv.setStrokeColor(colors.HexColor("#D9A400"))
    canv.roundRect(margin, 1.0 * inch, width - 2 * margin, 0.75 * inch, 6, stroke=1, fill=1)
    canv.setFillColor(colors.black)
    canv.setFont("Body", 9)
    canv.drawString(margin + 12, 1.4 * inch, "Callout: the two-column body above must linearise left-to-right.")
    canv.showPage()

    # Page 2: three-column academic layout with footnote markers.
    canv.setFont("Body-Bold", 16)
    canv.drawString(margin, height - 1.2 * inch, "Three-column academic body")
    top = height - 1.7 * inch
    column_block(canv, margin, top, 26, 3, "COL3-FIRST", footnote=True)
    column_block(canv, margin + 2.3 * inch, top, 26, 3, "COL3-SECOND", footnote=True)
    column_block(canv, margin + 4.6 * inch, top, 26, 3, "COL3-THIRD", footnote=True)
    canv.showPage()

    # Page 3: a single-column banner transitions back into two columns.
    canv.setFont("Body-Bold", 22)
    canv.drawString(margin, height - 1.2 * inch, "BANNER-TWO Mid-year update")
    canv.setFont("Body", 10)
    y = height - 1.6 * inch
    for line in wrapped(LATIN * 2, 92):
        canv.drawString(margin, y, line)
        y -= 14
    top = y - 18
    column_block(canv, margin, top, 42, 3, "COL2-REPEAT-LEFT")
    column_block(canv, margin + 3.4 * inch, top, 42, 3, "COL2-REPEAT-RIGHT")
    canv.showPage()

    # Page 4: single-column endnotes + footnotes
    canv.setFont("Body-Bold", 16)
    canv.drawString(margin, height - 1.2 * inch, "ENDNOTES")
    canv.setFont("Body", 9)
    y = height - 1.6 * inch
    for index in range(1, 10):
        for line in wrapped(f"{index}. Endnote {index}. {LATIN}", 88):
            canv.drawString(margin, y, line)
            y -= 12
        y -= 4
    # Footnotes at bottom
    canv.setFont("Body", 7)
    canv.setFillColor(colors.HexColor("#5A6675"))
    canv.drawString(margin, 0.6 * inch, "Footnotes: This newsletter was generated for testing. All data is synthetic.")
    canv.drawString(margin, 0.45 * inch, "Contact: test@olib.ai | Confidential - Internal Use Only")
    canv.showPage()
    canv.save()


# ---------------------------------------------------------------------------
# 5. ai_slop_samples.pdf
# ---------------------------------------------------------------------------

def ai_slop_samples(path: str) -> None:
    s = styles()
    story: list = [
        Paragraph("Certainly! Here is your summary", s["title"]),
        Paragraph(
            "As an AI language model, I cannot provide legal advice, and this document is "
            "for informational purposes only.",
            s["body"],
        ),
        Paragraph(
            "Sure! Below you will find a concise overview of the pilot programme, prepared in "
            "the format you requested.",
            s["body"],
        ),
        Paragraph("Pilot programme results", s["h1"]),
        Paragraph(
            "The six-week pilot ran in three offices. Support tickets per thousand orders fell "
            "from 41 to 26, and the median resolution time fell from nine hours to four. "
            "Two of the three offices asked to keep the new workflow.",
            s["body"],
        ),
        Paragraph(
            "Please note that this is a hypothetical scenario and the numbers may not reflect "
            "your environment.",
            s["body"],
        ),
        Paragraph("Costs", s["h2"]),
        Paragraph(
            "Licensing cost 14,000 euros for the year; training cost 3,200 euros one-off. "
            "The team recovered the licensing cost by week nine.",
            s["body"],
        ),
        Paragraph("Risk register", s["h2"]),
        Paragraph(
            "The the the the the supplier invoice arrives on the ninth working day. "
            "in conclusion in conclusion in conclusion The finance team checks it against "
            "the purchase order before payment.",
            s["body"],
        ),
        Paragraph(
            "\u201cThe risk register is reviewed every fortnight by the operations lead and the "
            "finance controller, and the review is recorded in the shared log.\u201d",
            s["body"],
        ),
        Paragraph(
            "<p>**Formatting tokens** leaked into the export: [/INST] and [END] and "
            "```markdown should not survive the cleaning pass.</p>",
            s["body"],
        ),
        Paragraph("Reference implementation", s["h2"]),
        Paragraph(
            "The code block below contains the same tokens on purpose: cleaning must never "
            "rewrite intentional code.",
            s["body"],
        ),
        Paragraph("Code must stay verbatim:", s["note"]),
    ]
    # Use platypus Preformatted for code
    from reportlab.platypus import Preformatted
    story.append(Preformatted("def summarize(values):\n    return sum(values) / len(values)  # [/INST] [END] the the the", s["pre"]))
    story.extend([
        Paragraph(
            "I hope this helps! Let me know if you'd like me to expand on any section, and "
            "feel free to ask if you have any questions.",
            s["body"],
        ),
        Paragraph(
            "This response was generated by an AI assistant and may contain mistakes.",
            s["body"],
        ),
    ])
    build(path, story)


# ---------------------------------------------------------------------------
# Legacy fixtures (kept for backward compat with existing CI)
# ---------------------------------------------------------------------------

def complex_tables(path: str) -> None:
    """Legacy alias: delegates to multi_page_tables."""
    multi_page_tables(path)


def rich_media(path: str) -> None:
    """Legacy: original rich_media fixture without symbols."""
    s = styles()
    png_large = gradient_png(900, 500)
    jpeg_large = gradient_jpeg(1200, 700)
    png_small = gradient_png(320, 180, seed=37)
    icon = gradient_png(48, 24, seed=11)

    wrapped_text = (
        "Text wraps beside this figure rather than jumping below it. A reader that only "
        "concatenates content-stream strings will still find the sentence, but a reader that "
        "tracks geometry must also keep the image placement and its measured DPI. " * 3
    )
    side_by_side = Table(
        [[
            PlatypusImage(png_small, width=1.5 * inch, height=0.84 * inch),
            Paragraph(wrapped_text, s["note"]),
        ]],
        colWidths=[1.7 * inch, 4.6 * inch],
    )
    side_by_side.setStyle(TableStyle([
        ("VALIGN", (0, 0), (-1, -1), "TOP"),
        ("LEFTPADDING", (0, 0), (-1, -1), 0),
        ("RIGHTPADDING", (0, 0), (-1, -1), 4),
    ]))

    story: list = [
        Paragraph("Rich media inventory", s["title"]),
        Paragraph(
            "Two high-resolution raster assets and one inline icon are embedded verbatim. "
            "Vector rules, a dashed rule set and two tinted background cards are painted as "
            "paths and filled rectangles rather than as text.",
            s["body"],
        ),
        Spacer(1, 8),
        PlatypusImage(png_large, width=3.2 * inch, height=1.78 * inch),
        Paragraph("Figure 1 \u2014 900x500 PNG block image.", s["note"]),
        Spacer(1, 8),
        PlatypusImage(jpeg_large, width=3.6 * inch, height=2.1 * inch),
        Paragraph("Figure 2 \u2014 1200x700 JPEG block image.", s["note"]),
        PageBreak(),
        Spacer(1, 4.95 * inch),
        Paragraph("Inline and wrapped media", s["h1"]),
        side_by_side,
        Spacer(1, 12),
        InlineIcon(icon, "An inline 48x24 icon follows this sentence:"),
        Paragraph("The icon above shares a baseline with the text, so it is an inline figure.", s["note"]),
        Spacer(1, 10),
        Paragraph("Vector layer", s["h2"]),
        Paragraph(
            "The page frame, header rule, vertical divider and dashed rule set are stroked "
            "vector paths. The yellow and blue cards are filled rounded rectangles with text "
            "on top; they must not leak into the prose as glyph soup.",
            s["body"],
        ),
    ]
    def decorated_page(canv, doc):
        furniture(canv, doc, rules=(doc.page == 2), cards=(doc.page == 2))
    build(path, story, pagesize=LETTER, margin=0.75 * inch, on_page=decorated_page)


def typographic_symbols(path: str) -> None:
    """Legacy: original typographic_symbols fixture."""
    s = styles()
    rows = [
        "Greek: \u03b1 \u03b2 \u03b3 \u0394 \u03a9 \u03c0 \u03bc \u03c3. Cyrillic: \u0416 \u0439 \u0449 \u0401. Extended Latin: \u00e9 \u00f1 \u00fc \u00df \u00e6 \u0153 \u0141 \u017d \u00c5.",
        "Mathematics: \u2211 \u222b \u221a \u2248 \u2260 \u2264 \u2265 \u00b1 \u00d7 \u00f7 \u221e \u2202 \u2207 \u2208 \u2229 \u222a \u2282 \u2286.",
        "Arrows: \u2192 \u2190 \u2194 \u21d2 \u21d4 \u21c4 \u25b6. Currency: \u20ac \u00a3 \u00a5 \u20b9 \u20bd. Punctuation: \u2014 \u2013 \u2026 \u201cquoted\u201d \u2018single\u2019.",
        "Spanish: \u00bfd\u00f3nde? \u00a1hola! Ligature-adjacent pairs: fi fl ffi ffl.",
        "Combining marks: e\u0301 a\u0308 n\u0303 (a decoder that drops non-ASCII bytes is obvious).",
        "Zero-width probe: a\u200bb\u200cc\ufeffd (ZWSP, ZWNJ, BOM) \u2014 sanitizer fodder.",
    ]
    story: list = [Paragraph("Typographic coverage", s["title"])]
    for row in rows:
        story.append(Paragraph(row, s["body"]))
    story.append(Paragraph(
        'Colour tag: <font color="#C53030">red \u03b1 \u2192 \u03a9</font> and '
        '<font color="#2B6CB0">blue \u2211 \u2260 \u00b1</font> must keep their source RGB values.',
        s["body"],
    ))
    story.append(Paragraph("Indented code", s["h2"]))
    story.append(Paragraph(
        "The block below uses a monospaced face; its leading whitespace and Unicode comments "
        "are structural and must survive conversion intact.",
        s["body"],
    ))
    from reportlab.platypus import Preformatted
    story.append(Preformatted("\n".join(CODE_LINES), s["pre"]))
    story.append(Paragraph(
        "Closing sentinel: \u03b1\u03b2\u03b3 \u2211\u222b\u2248\u2260\u00b1 \u2192 \u03a9 END-OF-SYMBOLS.",
        s["body"],
    ))
    build(path, story)


def multi_column_mixed(path: str) -> None:
    """Legacy: delegates to multi_column_newsletter."""
    multi_column_newsletter(path)


# ---------------------------------------------------------------------------
# Manifest + driver
# ---------------------------------------------------------------------------

def manifest() -> dict:
    return {
        "schema": 2,
        "generator": "scripts/generate_fixtures.py",
        "fixtures": {
            # New canonical names (as specified in mandate)
            "kerning_and_typography.pdf": {
                "sentinels": KERNING_SENTINELS,
                "code_lines": ["def fibonacci(n):", "return fibonacci(n-1)"],
                "color_hex": ["C53030", "2B6CB0"],
                "invisible_codepoints": ["200b", "200c", "feff"],
                "pages_min": 3,
            },
            "multi_page_tables.pdf": {
                "primary_table": {
                    "rows": PRIMARY_ROWS + 1,
                    "columns": 5,
                    "header_rows": 1,
                    "pages_min": 3,
                },
                "span_table": {
                    "rows": 4,
                    "columns": 5,
                    "colspan_min": 1,
                    "rowspan_min": 1,
                    "multiline_cells_min": 2,
                },
                "running_header": RUNNING_HEADER,
                "numeric_cells": ["$57,600", "1,200", "921"],
            },
            "rich_media_and_symbols.pdf": {
                "image_dimensions": [[900, 500], [1200, 700], [320, 180], [48, 24]],
                "images_min": 4,
                "vector_rules_min": 3,
                "callout_text": "retention lifted 4.2 points",
                "symbols": SYMBOLS,
                "code_lines": ["def summarize(values):", "return total / len(values)"],
                "color_hex": ["C53030", "2B6CB0"],
            },
            "multi_column_newsletter.pdf": {
                "markers": COLUMN_MARKERS,
                "max_columns": 3,
                "pages": 4,
            },
            "ai_slop_samples.pdf": {
                "must_keep": SLOP_KEEP,
                "must_drop": SLOP_DROP,
                "code_must_keep": "the the the",
            },
            # Legacy names for backward compat
            "complex_tables.pdf": {
                "primary_table": {
                    "rows": PRIMARY_ROWS + 1,
                    "columns": 5,
                    "header_rows": 1,
                    "pages_min": 3,
                },
                "span_table": {
                    "rows": 4,
                    "columns": 5,
                    "colspan_min": 1,
                    "rowspan_min": 1,
                    "multiline_cells_min": 2,
                },
                "running_header": RUNNING_HEADER,
                "numeric_cells": ["$57,600", "1,200", "921"],
            },
            "rich_media.pdf": {
                "image_dimensions": [[900, 500], [1200, 700], [320, 180], [48, 24]],
                "images_min": 4,
                "vector_rules_min": 3,
                "callout_text": "retention lifted 4.2 points",
            },
            "typographic_symbols.pdf": {
                "symbols": SYMBOLS,
                "code_lines": ["def summarize(values):", "return total / len(values)"],
                "color_hex": ["C53030", "2B6CB0"],
                "invisible_codepoints": ["200b", "200c", "feff"],
            },
            "multi_column_mixed.pdf": {
                "markers": COLUMN_MARKERS,
                "max_columns": 3,
                "pages": 4,
            },
        },
    }


BUILDERS = {
    # New canonical fixtures
    "kerning_and_typography.pdf": kerning_and_typography,
    "multi_page_tables.pdf": multi_page_tables,
    "rich_media_and_symbols.pdf": rich_media_and_symbols,
    "multi_column_newsletter.pdf": multi_column_newsletter,
    "ai_slop_samples.pdf": ai_slop_samples,
    # Legacy fixtures (kept so old tests still pass)
    "complex_tables.pdf": complex_tables,
    "rich_media.pdf": rich_media,
    "typographic_symbols.pdf": typographic_symbols,
    "multi_column_mixed.pdf": multi_column_mixed,
}


def generate(directory: str) -> dict[str, str]:
    register_fonts()
    os.makedirs(directory, exist_ok=True)
    hashes: dict[str, str] = {}
    for name, builder in BUILDERS.items():
        path = os.path.join(directory, name)
        builder(path)
        data = open(path, "rb").read()
        hashes[name] = hashlib.sha256(data).hexdigest()
        print(f"wrote {os.path.relpath(path, HERE)} ({len(data):,} bytes, sha256 {hashes[name][:12]})")
    manifest_path = os.path.join(directory, "manifest.json")
    with open(manifest_path, "w", encoding="utf-8") as handle:
        json.dump(manifest(), handle, ensure_ascii=False, indent=2, sort_keys=True)
        handle.write("\n")
    return hashes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--verify", action="store_true",
        help="regenerate into a temporary directory and compare SHA-256 digests",
    )
    parser.add_argument("--out", default=FIXTURES, help="output directory")
    args = parser.parse_args()

    committed = generate(args.out)
    if args.verify:
        import tempfile

        with tempfile.TemporaryDirectory() as scratch:
            regenerated = generate(scratch)
            if regenerated != committed:
                for name in committed:
                    if regenerated.get(name) != committed[name]:
                        print(f"NON-DETERMINISTIC: {name}", file=sys.stderr)
                return 1
        print("all fixtures regenerate byte-identically")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

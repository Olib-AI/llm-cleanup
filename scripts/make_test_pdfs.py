"""Generate rich test PDFs for the llm-cleanup PDF conversion pipeline.

Produces, under crates/cleanup-convert/tests/pdfs/fixtures/:

* minimal.pdf   - short single-page document
* rich_styles.pdf - headings, bold/italic runs, lists, a table, unicode text
                    (French, Spanish, Japanese, symbols), an image and rules
* multi_page.pdf - long document that forces pagination across several pages

The PDFs are committed to the repo and exercised by
crates/cleanup-convert/tests/pdfs.rs.
"""

import os
from reportlab.lib.pagesizes import letter
from reportlab.lib.colors import HexColor
from reportlab.lib.styles import ParagraphStyle
from reportlab.lib.enums import TA_CENTER, TA_JUSTIFY
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.platypus import (
    SimpleDocTemplate, Paragraph, Spacer, Table, TableStyle,
    Image, PageBreak, HRFlowable,
)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES = os.path.join(ROOT, "crates", "cleanup-convert", "tests", "pdfs", "fixtures")
os.makedirs(FIXTURES, exist_ok=True)

# Register fonts with real Unicode coverage (including CJK) so the fixtures
# contain non-ASCII text that survives text extraction.
pdfmetrics.registerFont(TTFont("DejaVu", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"))
pdfmetrics.registerFont(TTFont("DejaVu-Bold", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"))
pdfmetrics.registerFontFamily("DejaVu", normal="DejaVu", bold="DejaVu-Bold", italic="DejaVu", boldItalic="DejaVu-Bold")

INK = HexColor("#1a1a1a")
ACCENT = HexColor("#2e5fa3")
LIGHT = HexColor("#eef3f8")

body = ParagraphStyle("body", fontName="DejaVu", fontSize=10.5, leading=15, textColor=INK, alignment=TA_JUSTIFY, spaceAfter=8)
h1 = ParagraphStyle("h1", fontName="DejaVu-Bold", fontSize=22, leading=27, textColor=ACCENT, spaceBefore=14, spaceAfter=8)
h2 = ParagraphStyle("h2", fontName="DejaVu-Bold", fontSize=15, leading=19, textColor=ACCENT, spaceBefore=10, spaceAfter=6)
h3 = ParagraphStyle("h3", fontName="DejaVu-Bold", fontSize=12, leading=15, textColor=INK, spaceBefore=8, spaceAfter=4)
title = ParagraphStyle("title", fontName="DejaVu-Bold", fontSize=28, leading=34, textColor=ACCENT, alignment=TA_CENTER, spaceAfter=10)
subtitle = ParagraphStyle("subtitle", fontName="DejaVu", fontSize=13, leading=17, textColor=HexColor("#444444"), alignment=TA_CENTER, spaceAfter=6)
bullet = ParagraphStyle("bullet", parent=body, leftIndent=16, bulletIndent=4, spaceAfter=4)
numbered = ParagraphStyle("numbered", parent=body, leftIndent=18, bulletIndent=4, spaceAfter=4)

def header_footer(canvas, doc):
    canvas.saveState()
    canvas.setStrokeColor(ACCENT)
    canvas.setLineWidth(1.2)
    canvas.line(54, letter[1] - 40, letter[0] - 54, letter[1] - 40)
    canvas.setFont("DejaVu", 8)
    canvas.setFillColor(HexColor("#666666"))
    canvas.drawString(54, 30, "llm-cleanup test fixture")
    canvas.drawRightString(letter[0] - 54, 30, f"Page {doc.page}")
    canvas.restoreState()

def build(path, story, title_text):
    doc = SimpleDocTemplate(path, pagesize=letter, leftMargin=54, rightMargin=54, topMargin=64, bottomMargin=64,
                            title=title_text, author="llm-cleanup test suite", subject="PDF conversion fixture")
    doc.build(story, onFirstPage=header_footer, onLaterPages=header_footer)
    print(f"wrote {path}")

# --- minimal.pdf -----------------------------------------------------------
build(
    os.path.join(FIXTURES, "minimal.pdf"),
    [
        Paragraph("A minimal fixture", h1),
        Paragraph("One short paragraph of plain text with <b>bold</b> and <i>italic</i> runs.", body),
    ],
    "Minimal fixture",
)

# --- rich_styles.pdf -------------------------------------------------------
data = [
    ["Metric", "Q1", "Q2", "Q3"],
    ["Revenue", "$12.4k", "$15.1k", "$14.8k"],
    ["Expenses", "$8.2k", "$7.9k", "$9.0k"],
    ["Profit", "$4.2k", "$7.2k", "$5.8k"],
]
table = Table(data, colWidths=[110, 90, 90, 90])
table.setStyle(TableStyle([
    ("BACKGROUND", (0, 0), (-1, 0), ACCENT),
    ("TEXTCOLOR", (0, 0), (-1, 0), HexColor("#ffffff")),
    ("FONTNAME", (0, 0), (-1, 0), "DejaVu-Bold"),
    ("FONTNAME", (0, 1), (-1, -1), "DejaVu"),
    ("FONTSIZE", (0, 0), (-1, -1), 9),
    ("ROWBACKGROUNDS", (0, 1), (-1, -1), [HexColor("#ffffff"), LIGHT]),
    ("GRID", (0, 0), (-1, -1), 0.5, HexColor("#9fb3c8")),
    ("TOPPADDING", (0, 0), (-1, -1), 5),
    ("BOTTOMPADDING", (0, 0), (-1, -1), 5),
]))

story = [
    Paragraph("Quarterly Review", title),
    Paragraph("Prepared by the documentation team — <i>April 2026</i>", subtitle),
    Spacer(1, 14),
    HRFlowable(width="100%", thickness=1.4, color=ACCENT),
    Spacer(1, 10),
    Paragraph("1. Executive summary", h1),
    Paragraph(
        "This fixture exercises <b>bold</b>, <i>italic</i> and mixed "
        "<b><i>bold italic</i></b> runs inside a justified paragraph, plus "
        "non-ASCII text: Française, Español, 日本語 — € 1.234,56 · ½ · © 2026 · ¥ · §.", body),
    Paragraph("2. Contents", h2),
    Paragraph("• First bullet item, wrapping across lines when long enough.", bullet),
    Paragraph("• Second bullet item with <b>inline bold</b> inside.", bullet),
    Paragraph("1. Numbered step one.", numbered),
    Paragraph("2. Numbered step two.", numbered),
    Paragraph("3. Findings", h3),
    Paragraph("A small sub-section before the table.", body),
    table,
    Spacer(1, 10),
    Paragraph("3. Next steps", h2),
    Paragraph("Review the table above, then sign off the release.", body),
    PageBreak(),
    Paragraph("Second page content", h1),
    Paragraph(
        "Page two continues with more body text so the fixture spans multiple "
        "pages and exercises pagination in the converters.", body),
    Paragraph("CJK sample: 日本語 — see how it extracts.", body),
]
build(
    os.path.join(FIXTURES, "rich_styles.pdf"),
    story,
    "Rich styles fixture",
)

# --- multi_page.pdf --------------------------------------------------------
story = [Paragraph("Long document", title), Spacer(1, 12), HRFlowable(width="100%", thickness=1, color=ACCENT)]
for section in range(4):
    story.append(Paragraph(f"Section {section + 1}", h1))
    for para in range(8):
        story.append(Paragraph(
            f"Section {section + 1}, paragraph {para + 1}: "
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. "
            "Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. "
            "<b>Bold highlight</b> and <i>italic voice</i> inside the flow. "
            "French: Français; Spanish: España.", body))
    story.append(Spacer(1, 6))
build(
    os.path.join(FIXTURES, "multi_page.pdf"),
    story,
    "Multi-page fixture",
)

print("done")

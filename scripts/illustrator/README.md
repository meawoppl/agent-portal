# Illustrator pipeline

Tooling around [`docs/prompts/illustrate-change.md`](../../docs/prompts/illustrate-change.md),
which has a Fable session read a PR and compose an SVG explaining what the
change does. These scripts are the parts worth automating: checking that the
output follows the canonical layout, and assembling many diagrams into one
sheet.

The diagram itself is deliberately **not** automated. An earlier attempt
generated diagrams from git statistics and could only ever draw where bytes
moved — file-churn treemaps that explained nothing. Deciding what a change
*means* is the whole job, and it is the part a script cannot do.

## Generate

Run the prompt in a Fable session per PR, writing each SVG to a directory:

```
/tmp/diagrams/pr-1643.svg
/tmp/diagrams/pr-1659.svg
...
```

## Check

```sh
./check-layout.py '/tmp/diagrams/pr-*.svg'
```

Verifies the canonical layout mechanically — canvas, background rect, header
and subtitle baselines, `PR #n · title` heading format, the rule, the dashed
divider, that content actually fills the panel band, and that every `id` is
prefixed with its PR number. Prints a table and a failure tally.

Sessions are independent and cannot see each other's output, so "they all
looked consistent" is not something to take on faith across a dozen of them.

Pair it with [`../check-svg-glyphs.py`](../check-svg-glyphs.py), which catches
characters the rendering font cannot draw — those appear as empty boxes rather
than raising an error.

## Assemble

```sh
./make-contact-sheet.py 1643,1659,1611,1639,1649,1650,1576,1568,1613,1612,1661,1660 \
    /tmp/diagrams /tmp/diagrams/contact-sheet.svg
```

Tiles twelve diagrams 3×4 with rules between them.

**It namespaces every `id` per tile**, which is the non-obvious part. Bare ids
like `arrow-green` collide across files — three of twelve diagrams defined
exactly that — and on collision the first definition in document order wins, so
later tiles silently draw the wrong marker. Nothing errors; the arrowheads are
just the wrong colour.

## Export

```sh
cairosvg contact-sheet.svg -o sheet-4k.png --output-width 3840
```

Verify opacity by rendering over white and sampling a corner — on a dark
viewer a transparent background and an opaque one look identical:

```sh
cairosvg sheet.svg -o /tmp/check.png --background "#ffffff"
python3 -c "from PIL import Image; print(Image.open('/tmp/check.png').convert('RGB').getpixel((3,3)))"
# expect (26, 27, 38)
```

## One caution

These sessions write a draft, rasterize it, look at it, and revise. A file
appearing on disk does **not** mean the session is finished, so assembling on
file existence can capture a superseded tile. Wait for the session to report
completion; this bit twice, and only comparing mtimes afterwards caught it.

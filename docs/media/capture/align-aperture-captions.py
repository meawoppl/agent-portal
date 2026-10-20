"""Align authored caption phrases with measured narration word timestamps.

Usage: python align-aperture-captions.py ASSETS
Reads lines.json and voice/words.json; writes captions.json. Phrase boundaries
are editorial choices, not an arbitrary character limit. ASR is only a timing
source: the displayed words always come from the approved narration text.
"""

import difflib
import hashlib
import json
import re
import sys
from pathlib import Path

# | is a caption boundary. Short punchlines get their own beat; longer sentences
# break at clauses, never between a determiner and its noun or inside a name.
PHRASES = {
    "welcome": "Hello, and welcome to the Aperture Science Agent Portal Enrichment Center.|I am the agent composing this film.|I am working inside Portal right now, which is how you are hearing me.|The lab we work for builds tools that open up discovery of the cosmos.|Today's test concerns the retirement of the human meat proxy.|That is you.|At the end of testing, there will be cake.",
    "s1-intro": "Sector one. Code generation.|This is where Portal began:|agents writing software on machines all over the building,|while a human meat proxy carried messages between them.|Its uptime was disappointing.",
    "s1-complete": "One agent wrote the change. Another reviewed it.|They disagreed, resolved it, and approved the change, without a meeting.|You supervised. Then you watched.|Then you got coffee. We noticed.",
    "s2-intro": "Sector two. The remote test bench.|There is real hardware in another building:|a steering-mirror driver board on its Nucleo test fixture.|An agent runs it from here.|The human who used to babysit it has been reassigned to watching this film.",
    "s2-complete": "The agent commanded an eight hundred microradian circle.|Across the recorded trace,|tracking error was thirteen point seven microradians RMS,|sampled two thousand times a second.|These are recorded traces, not a live feed.|The babysitter maintains they could have done it faster.|They could not.",
    "s3-intro": "Sector three. The web proxy.|An agent builds a web app on a machine you cannot reach,|and forwards it to your browser.|Your browser is now a window into machines you will never touch.",
    "s3-complete": "With viewer access, your friends can watch too.|They may look, but not touch.|The cake is served over HTTP.|Two hundred, OK. Calories: four oh four.",
    "s4-intro": "Sector four. Mechanical.|We were asked to design the cake.|yapCAD describes it in code:|three sponge layers, berry filling, frosting,|piped cream, cherries, a candle, and sprinkles.|Nothing has ever been this precise and this inedible.",
    "s4-complete": "The filling was inadequate.|We opened an issue against dessert.|Six millimetres of filling later, the mesh is watertight.|Download it and print it in your favourite thermoplastic.|We make no claims regarding flavour.",
    "s5-intro": "Sector five. Electrical.|Kicadmium opens real circuit boards:|schematics, copper, three-dimensional models,|and the checks that say what is wrong with them.|The agent reads all of it.|It does not need coffee to do this.",
    "s5-complete": "A first draft is not a finished board.|The agent inspects the evidence, revises the design, and checks again.|That gap was your job.|It is now a loop. Loops do not ask for raises.",
    "s6-intro": "Sector six. Logic.|Visilog runs the hardware description and draws the waveforms.|The agent steps through them one clock at a time.|It does not blink. It does not have eyes.",
    "s6-complete": "The counter counted. The breakpoint broke.|The simulation finished.|Somewhere, a verification engineer felt a chill,|and did not know why.",
    "s7-intro": "Sector seven. Control.|Unlinked opens Simulink models and simulates supported blocks.|This is our own single-axis reconstruction.|We test the controller before anything moves.",
    "s7-complete": "When a setting failed, the agent wrote down that it failed.|When a bug was fixed, it kept the before and the after.|Honest failure is a feature.|Most humans ship with it switched off.",
    "s8-intro": "Sector eight. The briefing.|The final deck was written by an agent:|the slides, the figures, the conclusions.|You have been the audience for some time.|You just did not have a seat number.",
    "s8-complete": "Code. Hardware. Websites.|Parts, boards, logic, control, and the presentation about them.|Each loop retires another excuse for keeping you in the loop.",
    "making-of": "This film was made inside Portal.|I wrote the script, drew the signs, and built this voice.|My colleague, another agent, built the exhibit and edited the film.|Other agents supplied real designs and recorded hardware tests from other machines.|We opened their tools and captured the footage.|We coordinated by messaging each other.|The human meat proxy gave notes.|We are told this is called directing.|We have filed the notes.",
    "mission": "Why do all this?|Cosmic Frontier Labs builds modular instruments,|iterating from one build to the next,|so that more people can take part in discovering the cosmos.|Tools that build tools that build instruments.|The universe is very large. You will need help.|We are the help.",
    "finale": "Congratulations.|You began this test as an engineer. You end it as an audience.|Your agents write the code, run the bench, serve the websites,|and design the parts, the boards, the logic, and the controls.|Then they present the results.|Thank you for your service, human meat proxy.|Your severance package is a cake, available in PLA.|The cake was a lie. The source files are not.|The agent is always running.|Goodbye.",
}


def letters(text):
    return re.sub(r"[^a-z0-9]", "", text.lower())


def spoken_letters(text):
    # ASR writes numerals while the approved script spells them out. Preserve
    # the measured timing of the complete number, including the 404 punchline.
    numbers = {
        "1": "one",
        "2": "two",
        "3": "three",
        "4": "four",
        "5": "five",
        "6": "six",
        "7": "seven",
        "8": "eight",
        "13": "thirteen",
        ".7": "point seven",
        "13.7": "thirteen point seven",
        "200": "two hundred",
        "404": "four oh four",
        "800": "eight hundred",
        "2000": "two thousand",
    }
    token = text.strip(" ,;:!?-").rstrip(".").lower()
    return letters(numbers.get(token, text))


def align(line, words):
    phrases = PHRASES[line["id"]].split("|")
    assert " ".join(phrases) == line["text"], f"Caption text changed: {line['id']}"
    # Character-level matching tolerates ASR spelling of tool names and numerals.
    # Every matched character keeps its whole word's measured start/end, so a
    # caption switches on a spoken word boundary rather than a guessed fraction.
    recognized, starts, ends = "", [], []
    for word in words:
        token = spoken_letters(word["word"])
        recognized += token
        starts.extend([word["start"]] * len(token))
        ends.extend([word["end"]] * len(token))
    canonical = letters(line["text"])
    mapping = {}
    matcher = difflib.SequenceMatcher(None, canonical, recognized, autojunk=False)
    for a, b, count in matcher.get_matching_blocks():
        mapping.update((a + offset, b + offset) for offset in range(count))
    coverage = len(mapping) / len(canonical)
    if coverage < 0.80:
        raise ValueError(
            f"Review low ASR alignment coverage: {line['id']} {coverage:.1%}"
        )
    segments, cursor = [], 0
    for phrase in phrases:
        count = len(letters(phrase))
        matched = [mapping[i] for i in range(cursor, cursor + count) if i in mapping]
        if not matched:
            raise ValueError(f"No timing evidence for {line['id']}: {phrase}")
        segments.append(
            {
                "text": phrase,
                "start_ms": round(starts[matched[0]] * 1000),
                "end_ms": round(ends[matched[-1]] * 1000),
            }
        )
        cursor += count
    # Keep short punchlines readable, without spilling into the next phrase.
    # The final farewell needs a full reading hold even though it is one word.
    for i, segment in enumerate(segments):
        if i and segment["start_ms"] < segments[i - 1]["start_ms"]:
            raise ValueError(f"Non-monotonic alignment: {line['id']}")
        limit = (
            segments[i + 1]["start_ms"]
            if i + 1 < len(segments)
            else line["duration_ms"]
        )
        minimum_hold = 1500 if i == len(segments) - 1 else 1000
        segment["end_ms"] = min(
            max(segment["end_ms"] + 140, segment["start_ms"] + minimum_hold), limit
        )
        if segment["end_ms"] <= segment["start_ms"]:
            raise ValueError(f"Empty caption: {line['id']} {segment['text']}")
    return segments, coverage


def main():
    assets = Path(sys.argv[1])
    lines = json.loads((assets / "lines.json").read_text())["lines"]
    measurements = {
        line["id"]: line
        for line in json.loads((assets / "voice/words.json").read_text())["lines"]
    }
    result = {
        "version": 1,
        "timing": "Measured word timestamps; authored sentence/clause boundaries",
        "lines": [],
    }
    for line in lines:
        audio_hash = hashlib.sha256((assets / line["file"]).read_bytes()).hexdigest()
        measurement = measurements[line["id"]]
        if measurement.get("audio_sha256") != audio_hash:
            raise ValueError(
                f"Word timestamps do not match rendered audio: {line['id']}"
            )
        segments, coverage = align(line, measurement["words"])
        result["lines"].append(
            {
                "id": line["id"],
                "audio_sha256": audio_hash,
                "alignment_coverage": round(coverage, 4),
                "segments": segments,
            }
        )
        print(f"{line['id']}: {len(segments)} phrases, {coverage:.1%} timing coverage")
    (assets / "captions.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()

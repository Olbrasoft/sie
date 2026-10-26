#!/usr/bin/env python3
"""Score deliberately supplied benchmark audio; never access dictation history."""
import argparse
import json
import re
from pathlib import Path


def normalized_words(text):
    # Preserve Czech diacritics and digits; ignore case and punctuation only.
    return re.findall(r"\w+", text.casefold())


def edit_distance(reference, hypothesis):
    previous = list(range(len(hypothesis) + 1))
    for i, left in enumerate(reference, 1):
        current = [i]
        for j, right in enumerate(hypothesis, 1):
            current.append(min(
                current[-1] + 1,
                previous[j] + 1,
                previous[j - 1] + (left != right),
            ))
        previous = current
    return previous[-1]


def score(report, manifest):
    references = {s["sample"]: s["reference"] for s in manifest["samples"]}
    measured = [m for m in report["measurements"] if m["phase"] == "measured"]
    if {Path(m["sample"]).name for m in measured} != set(references):
        raise ValueError("References must match all measured sample names exactly")
    results = []
    for enabled in (False, True):
        words = characters = word_errors = character_errors = 0
        calls = []
        for measurement in measured:
            if measurement["flash_attention"] != enabled:
                continue
            name = Path(measurement["sample"]).name
            ref = normalized_words(references[name])
            hyp = normalized_words(measurement["text"])
            ref_chars, hyp_chars = " ".join(ref), " ".join(hyp)
            if not ref:
                raise ValueError("A reference must contain words")
            errors = edit_distance(ref, hyp)
            char_errors = edit_distance(ref_chars, hyp_chars)
            words += len(ref)
            characters += len(ref_chars)
            word_errors += errors
            character_errors += char_errors
            calls.append({"sample": name, "block": measurement["block"],
                          "word_errors": errors, "character_errors": char_errors})
        if not calls:
            raise ValueError("Both Flash Attention settings must be measured")
        results.append({
            "flash_attention": enabled,
            "unique_reference_words": sum(len(normalized_words(t)) for t in references.values()),
            "evaluated_reference_words": words,
            "evaluated_reference_characters": characters,
            "word_errors": word_errors,
            "character_errors": character_errors,
            "wer_percent": 100 * word_errors / words,
            "cer_percent": 100 * character_errors / characters,
            "sum_sample_medians_seconds": sum(s["median_seconds"] for s in report["summary"]
                                              if s["flash_attention"] == enabled),
            "calls": calls,
        })
    return {"normalization": "Unicode casefold and word tokens; diacritics retained; CER includes normalized spaces",
            "results": results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = score(json.loads(args.report.read_text()), json.loads(args.manifest.read_text()))
    text = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
    if args.output:
        with args.output.open("x") as destination:
            destination.write(text)
    else:
        print(text, end="")


if __name__ == "__main__":
    main()

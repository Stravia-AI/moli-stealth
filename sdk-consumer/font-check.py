"""在独立 Fontconfig 环境运行真实 SDK 字体正例和错误字体负例。"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.request
import xml.etree.ElementTree as xml

FONTS = {
    "positive": (
        "NotoSansCJKsc-Regular.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/523d033d6cb47f4a80c58a35753646f5c3608a78/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf",
        "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b",
        "Noto Sans CJK SC",
    ),
    "negative": (
        "NotoSerifCJKsc-Regular.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/9b0f1436e455d902de067a2501422e5dc71ad16b/Serif/OTF/SimplifiedChinese/NotoSerifCJKsc-Regular.otf",
        "2a2eae2628df83556c54018c41e20fa532c1b862c5256ae8b3f23feb918d12ca",
        "Noto Serif CJK SC",
    ),
}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--consumer", required=True, type=Path)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    if "linux" not in args.target:
        parser.error("font isolation requires a native Linux consumer")
    consumer = args.consumer.resolve()
    executable = consumer / "target" / args.target / "release" / "moli-sdk-consumer"
    evidence = consumer / "evidence" / "font-isolation"
    evidence.mkdir(parents=True, exist_ok=True)
    latin = Path(subprocess.check_output(["fc-match", "-f", "%{file}", "DejaVu Sans"], text=True))
    if not latin.is_file():
        raise RuntimeError("DejaVu Sans is unavailable in the runtime environment")
    results = []
    with tempfile.TemporaryDirectory(prefix="sdk-font-isolation-") as temporary:
        root = Path(temporary)
        for case, (filename, url, expected, family) in FONTS.items():
            fonts = root / case
            fonts.mkdir()
            shutil.copyfile(latin, fonts / "DejaVuSans.ttf")
            font = fonts / filename
            digest = hashlib.sha256()
            total = 0
            with urllib.request.urlopen(url, timeout=60) as response, font.open("wb") as output:
                while block := response.read(1024 * 1024):
                    total += len(block)
                    if total > 64 * 1024 * 1024:
                        raise RuntimeError("font fixture exceeds 64 MiB")
                    digest.update(block)
                    output.write(block)
            if digest.hexdigest() != expected:
                raise RuntimeError(f"font fixture SHA-256 mismatch: {url}")
            config = xml.Element("fontconfig")
            xml.SubElement(config, "dir").text = str(fonts)
            xml.SubElement(config, "cachedir").text = str(root / (case + "-cache"))
            if case == "negative":
                alias = xml.SubElement(config, "alias", {"binding": "strong"})
                xml.SubElement(alias, "family").text = "Noto Sans CJK SC"
                xml.SubElement(xml.SubElement(alias, "prefer"), "family").text = family
            configuration = root / (case + ".conf")
            xml.ElementTree(config).write(configuration, encoding="utf-8", xml_declaration=True)
            case_evidence = evidence / case
            case_evidence.mkdir()
            shutil.copyfile(configuration, case_evidence / "fontconfig.conf")
            env = os.environ | {
                "FONTCONFIG_FILE": str(configuration),
                "FONTCONFIG_PATH": str(root),
                "MOLI_SDK_EVIDENCE_DIR": str(case_evidence),
            }
            selected = subprocess.check_output(["fc-match", "-f", "%{family}", "Noto Sans CJK SC"], env=env, text=True)
            if family not in selected.split(","):
                raise RuntimeError(f"isolated {case} selected {selected!r}, expected {family}")
            result = subprocess.run([str(executable), "fonts"], env=env, text=True, capture_output=True)
            (case_evidence / "stdout.log").write_text(result.stdout, encoding="utf-8")
            (case_evidence / "stderr.log").write_text(result.stderr, encoding="utf-8")
            comparison_path = case_evidence / "font-outline-comparison.json"
            if not comparison_path.is_file():
                raise RuntimeError(f"{case} failed before the independent glyph comparison: {result.stderr}")
            comparison = json.loads(comparison_path.read_text())
            should_match = case == "positive"
            results.append({"case": case, "exit_code": result.returncode, "font_sha256": expected,
                            "font_url": url, "selected_family": selected, "comparison": comparison})
            (evidence / "results.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
            if (result.returncode == 0) != should_match or comparison["matches"] != should_match:
                raise RuntimeError(f"{case} did not satisfy the font identity contract: {result.stderr}")
            print(f"{case}: glyph comparison matched={should_match}, exit={result.returncode}", flush=True)


if __name__ == "__main__":
    main()

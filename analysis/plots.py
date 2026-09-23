"""Figures for the fill-model study. Reads artifacts/, writes figures/.

    uv run --with matplotlib python analysis/plots.py

Only matplotlib and the standard library: the numbers are computed in Rust and
written to artifacts/, and nothing here recomputes them. A figure that disagrees
with the artifact is a bug in this file, and the titles quote the artifact so
that disagreement is visible rather than silent.
"""

import csv
import json
import pathlib
from collections import defaultdict

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

INK, ACCENT, MUTED = "#1f3a5f", "#b03a2e", "#8a94a6"
MODELS = ["naive", "pessimistic", "proportional"]
LABELS = {"naive": "fill at touch", "pessimistic": "queue, pessimistic",
          "proportional": "queue, proportional"}

plt.rcParams.update({
    "font.family": "serif", "font.size": 9,
    "axes.edgecolor": "#2b3440", "axes.linewidth": 0.8,
    "axes.grid": True, "grid.alpha": 0.25, "grid.linewidth": 0.5,
    "axes.axisbelow": True, "axes.spines.top": False, "axes.spines.right": False,
    "figure.dpi": 110, "savefig.dpi": 300, "savefig.bbox": "tight",
})

ROOT = pathlib.Path(__file__).resolve().parent.parent
ART, FIG = ROOT / "artifacts", ROOT / "figures"


def load():
    with (ART / "runs.csv").open() as f:
        rows = list(csv.DictReader(f))
    for r in rows:
        r["pnl_btc"] = float(r["pnl_btc"])
        r["fills"] = int(r["fills"])
        r["markout_1s_bps"] = float(r["markout_1s_bps"])
    headline = json.loads((ART / "headline.json").read_text())
    return rows, headline


def by_day(rows, quoter, model, field):
    out = {}
    for r in rows:
        if r["quoter"] == quoter and r["model"] == model:
            out[r["day"]] = r[field]
    return out


def fig_fills_and_pnl(rows, headline):
    days = sorted({r["day"] for r in rows})
    x = range(len(days))
    fig, axes = plt.subplots(1, 2, figsize=(7.6, 3.4))

    ax = axes[0]
    for model, colour in zip(MODELS, [ACCENT, INK, MUTED]):
        series = by_day(rows, "touch", model, "fills")
        ax.plot(x, [series.get(d, 0) for d in days], marker="o", ms=3.4, lw=1.3,
                color=colour, label=LABELS[model])
    ax.set_xticks(list(x))
    ax.set_xticklabels(days, rotation=45, ha="right", fontsize=7)
    ax.set_ylabel("fills per day")
    inflation = headline.get("fill_inflation_mean")
    lo, hi = headline.get("fill_inflation_ci95", [float("nan")] * 2)
    ax.set_title(
        f"Fill-at-touch hands out {inflation:.2f}x the fills\n"
        f"95% CI {lo:.2f} to {hi:.2f}, "
        f"{headline.get('days', 0)} day{'' if headline.get('days') == 1 else 's'}",
        loc="left", fontsize=9.5)
    ax.legend(fontsize=7.4, frameon=False)

    ax = axes[1]
    for model, colour in zip(MODELS, [ACCENT, INK, MUTED]):
        series = by_day(rows, "touch", model, "pnl_btc")
        ax.plot(x, [series.get(d, float("nan")) for d in days], marker="o", ms=3.4,
                lw=1.3, color=colour, label=LABELS[model])
    ax.axhline(0, color="#2b3440", linewidth=0.8)
    ax.set_xticks(list(x))
    ax.set_xticklabels(days, rotation=45, ha="right", fontsize=7)
    ax.set_ylabel("P&L, BTC")
    sign = "flatters" if headline.get("naive_pnl_positive") else "exaggerates the loss of"
    ax.set_title(f"and therefore {sign} the strategy", loc="left", fontsize=9.5)

    fig.tight_layout()
    fig.savefig(FIG / "fills_and_pnl.png")
    plt.close(fig)


def fig_markouts(rows):
    groups = defaultdict(list)
    for r in rows:
        groups[(r["quoter"], r["model"])].append(r["markout_1s_bps"])
    keys = sorted(groups)
    means = [sum(groups[k]) / len(groups[k]) for k in keys]

    fig, ax = plt.subplots(figsize=(7.4, 3.4))
    colours = [ACCENT if k[1] == "naive" else INK for k in keys]
    ax.bar([f"{q}\n{LABELS[m]}" for q, m in keys], means, color=colours)
    ax.axhline(0, color="#2b3440", linewidth=0.8)
    ax.set_ylabel("mean markout at 1s, bps")
    ax.set_title("Adverse selection by quoter and fill model", loc="left", fontsize=10)
    ax.tick_params(axis="x", labelsize=6.8)
    fig.tight_layout()
    fig.savefig(FIG / "markouts.png")
    plt.close(fig)


def main():
    FIG.mkdir(exist_ok=True)
    rows, headline = load()
    if not rows:
        raise SystemExit("artifacts/runs.csv is empty: run `make study` on recorded days")
    fig_fills_and_pnl(rows, headline)
    fig_markouts(rows)
    for p in sorted(FIG.glob("*.png")):
        print(f"wrote figures/{p.name}  ({p.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    main()

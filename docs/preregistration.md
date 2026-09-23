# Preregistration

## Hypotheses, fixed before the study runs

- **H1.** Naive fill-at-touch overstates market-making P&L relative to a queue-aware
  model. Reported as a percentage with a 95% confidence interval from a paired bootstrap
  over trading days.
- **H2.** The dispersion advantage of inventory skew over symmetric quoting shrinks
  under queue-aware fills, relative to what `vol-lab` measured on synthetic paths.
- **H3.** Fills that occur under the queue-aware model are worse on markouts than fills
  under the naive model, because the queue clears exactly when the price is about to move
  through it.

A null result on any of these is published in the README with the same prominence as a
positive one.

## Fixed before any result was seen

Written 2026-09-22, before the recorder collected a single message.

- Instrument: BTC-PERPETUAL. Feed: public `100ms` book, `100ms` trades, `quote`.
- Sample: every complete UTC day recorded before the study runs, minimum 7 days.
  Days with a detected gap totalling more than 60 seconds are excluded, and the
  exclusion is reported.
- Primary statistic: (naive P&L - queue-aware P&L) / |naive P&L|, per day, aggregated
  by a paired bootstrap over days, 10,000 resamples, percentile 95% interval.
- Secondary: the ratio of P&L standard deviation between symmetric and inventory-skew
  quoting under each fill model, compared with the ratio `vol-lab` measured on synthetic
  paths.
- Fill-model parameters are fixed here: queue-pessimistic and queue-proportional as
  defined in the plan, no tuning after seeing results.
- If the primary interval contains zero, the README says so in the headline.

## Amendment, 2026-09-23

Made before any day was collected, on four minutes of live data recorded only to test that
the pipeline runs, and with no result computed from it.

The primary statistic moves from the GLFT quoter to a quoter that joins the touch.

The reason is mechanical rather than a preference. With the intensity fitted from that
sample, A 1.96 and kappa 0.2186, the GLFT half-spread is several ticks wide, so its quotes
rest well behind the best bid and ask. A quote resting behind the touch is filled almost
entirely by trades sweeping through it, and a sweep fills an order whatever its queue
position was. All three fill models therefore produced byte identical P&L, which makes the
comparison the study exists to run vacuous rather than negative.

Joining the touch is also what the hypothesis is about: H1 asks how much a backtest
overstates P&L by assuming you are filled whenever a trade prints at your price, and that
assumption only bites for an order resting at a price where trades print.

The GLFT and symmetric quoters stay in the grid and are still reported. Nothing else in
this document changes, and no fill-model parameter was tuned.

One statistic is added rather than replaced: the ratio of fills under the naive model to
fills under the queue-aware one. The P&L difference changes sign with the profitability of
the strategy, since a model that hands out fills that were never there exaggerates a loss
just as it flatters an edge, and the fill ratio does not. Both are reported, and the sign
of the mean naive P&L is reported next to them so the reader can tell which case they are
in.

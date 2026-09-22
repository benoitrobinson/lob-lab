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

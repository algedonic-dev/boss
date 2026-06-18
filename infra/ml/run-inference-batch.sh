#!/usr/bin/env bash
# Daily inference-batch trigger for boss-ml's active heuristic-formula
# and declarative-rule models. Step 7 of the Phase 2 cutover hooks
# this into systemd via boss-ml-inference-batch.timer (see
# boss-ml-inference-batch.service alongside).
#
# Usage:
#   BOSS_ML_API_URL=http://127.0.0.1:7070 ./run-inference-batch.sh
#
# Iterates the model id list and POSTs infer-batch on each one. The
# response body (BatchInferReport) is logged but not parsed; failures
# bubble up as non-zero exit so systemd records them.

set -euo pipefail

BASE="${BOSS_ML_API_URL:-http://127.0.0.1:7070}"
MODELS=(
  # Order matters: the churn-risk plugin must populate
  # ml_predictions BEFORE next-action-high-churn-risk reads them.
  mdl-account-churn-risk-v1
  mdl-next-action-contract-expiring-v1
  mdl-next-action-past-due-invoice-v1
  mdl-next-action-missing-primary-contact-v1
  mdl-next-action-high-churn-risk-v1
  mdl-next-action-stalled-service-ticket-v1
  # Renamed from mdl-next-action-pm-visit-due-v1 in v1.0.6 ML
  # plugin refresh; registry now exposes the verbose name.
  mdl-next-action-preventive-maintenance-due-v1
)

for id in "${MODELS[@]}"; do
  echo "==> infer-batch ${id}"
  curl -sSf -X POST "${BASE}/api/ml/models/${id}/infer-batch"
  echo
done

#!/bin/bash
set -e

# Runtime secrets are read from your shell (or .env) and passed to Cloud Run.
# Nothing sensitive is stored in this file.
if [ -f .env ]; then
    set -a
    # shellcheck disable=SC1091
    . ./.env
    set +a
fi

PROJECT=${GCP_PROJECT:?set GCP_PROJECT to your Google Cloud project id}
: "${SUPABASE_DB_URL:?set SUPABASE_DB_URL}"
IMAGE=gcr.io/$PROJECT/oddsmith
REGION=${REGION:-us-east1}
SERVICE=oddsmith

ENV_VARS="SUPABASE_DB_URL=${SUPABASE_DB_URL}"
if [ -n "${SUPABASE_URL:-}" ]; then ENV_VARS="${ENV_VARS},SUPABASE_URL=${SUPABASE_URL}"; fi
if [ -n "${SUPABASE_ANON_KEY:-}" ]; then ENV_VARS="${ENV_VARS},SUPABASE_ANON_KEY=${SUPABASE_ANON_KEY}"; fi

echo "==> Submitting container build to Cloud Build..."
gcloud builds submit --project "$PROJECT" --tag "$IMAGE"

echo "==> Deploying to Cloud Run..."
gcloud run deploy "$SERVICE" \
    --project "$PROJECT" \
    --image "$IMAGE" \
    --set-env-vars "$ENV_VARS" \
    --port 8080 \
    --timeout 3600 \
    --allow-unauthenticated \
    --region "$REGION" \
    --memory 512Mi

echo "==> Deployed!"

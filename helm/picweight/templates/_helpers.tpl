{{/*
Expand the name of the chart.
*/}}
{{- define "picweight.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "picweight.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Create chart label.
*/}}
{{- define "picweight.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels.
*/}}
{{- define "picweight.labels" -}}
helm.sh/chart: {{ include "picweight.chart" . }}
{{ include "picweight.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels.
*/}}
{{- define "picweight.selectorLabels" -}}
app.kubernetes.io/name: {{ include "picweight.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Name of the Secret holding OIDC credentials
(client_id / client_secret / mobile_client_id).

The chart NEVER creates this Secret (PRD §10) — it only references one by name.
Defaulting to "<fullname>-oidc" means the SealedSecret / ExternalSecret examples
in values.yaml work with no further configuration.
*/}}
{{- define "picweight.oidcSecretName" -}}
{{- if .Values.oidc.existingSecret }}
{{- .Values.oidc.existingSecret }}
{{- else }}
{{- printf "%s-oidc" (include "picweight.fullname" .) }}
{{- end }}
{{- end }}

{{/*
Name of the Secret holding the OpenAI API key. Same contract as the OIDC one:
referenced, never generated.
*/}}
{{- define "picweight.openaiSecretName" -}}
{{- if .Values.openai.existingSecret }}
{{- .Values.openai.existingSecret }}
{{- else }}
{{- printf "%s-openai" (include "picweight.fullname" .) }}
{{- end }}
{{- end }}

{{/*
Name of the data PVC (SQLite + thumbs/).
*/}}
{{- define "picweight.dataClaimName" -}}
{{- if .Values.persistence.existingClaim }}
{{- .Values.persistence.existingClaim }}
{{- else }}
{{- printf "%s-data" (include "picweight.fullname" .) }}
{{- end }}
{{- end }}

{{/*
Effective OIDC redirect URI for the confidential web client.
*/}}
{{- define "picweight.oidcRedirectUri" -}}
{{- if .Values.oidc.redirectUri }}
{{- .Values.oidc.redirectUri }}
{{- else }}
{{- printf "https://%s/api/auth/callback" .Values.ingress.host }}
{{- end }}
{{- end }}

{{/* SPDX-License-Identifier: Apache-2.0 */}}

{{/* Chart name, truncated to the 63-character label limit. */}}
{{- define "secretspec-operator.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Fully qualified app name: the release name when it already contains the
chart name, else "<release>-<chart>". Also the leader-election lease name.
*/}}
{{- define "secretspec-operator.fullname" -}}
{{- if contains .Chart.Name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{/* Chart label value. */}}
{{- define "secretspec-operator.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Selector labels. */}}
{{- define "secretspec-operator.selectorLabels" -}}
app.kubernetes.io/name: {{ include "secretspec-operator.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/* Common labels. */}}
{{- define "secretspec-operator.labels" -}}
helm.sh/chart: {{ include "secretspec-operator.chart" . }}
{{ include "secretspec-operator.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: secretspec-operator
{{- end }}

{{/* ServiceAccount name. */}}
{{- define "secretspec-operator.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "secretspec-operator.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- required "serviceAccount.name is required when serviceAccount.create is false" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Image reference: "<registry>/<repository>@<digest>" when image.digest is set,
else "<registry>/<repository>:<tag>". <registry> is global.imageRegistry when
non-empty, else image.registry. The tag defaults to the chart appVersion
unchanged: release tags, image tags, chart version and appVersion are the same
semver string (no "v" prefix; only 0.1.0 was published as "v0.1.0").
*/}}
{{- define "secretspec-operator.image" -}}
{{- $registry := .Values.image.registry }}
{{- with .Values.global }}{{- with .imageRegistry }}{{- $registry = . }}{{- end }}{{- end }}
{{- if .Values.image.digest }}
{{- printf "%s/%s@%s" $registry .Values.image.repository .Values.image.digest }}
{{- else }}
{{- $tag := .Values.image.tag | default .Chart.AppVersion }}
{{- if eq $tag "latest" }}
{{- fail "image.tag must not be \"latest\"; use a version tag or image.digest" }}
{{- end }}
{{- printf "%s/%s:%s" $registry .Values.image.repository $tag }}
{{- end }}
{{- end }}

{{/* Cross-field validation of the values. */}}
{{- define "secretspec-operator.validate" -}}
{{- if and (not .Values.leaderElection.enabled) (gt (int .Values.replicas) 1) }}
{{- fail (printf "replicas=%d requires leaderElection.enabled=true: without leader election several replicas would reconcile concurrently" (int .Values.replicas)) }}
{{- end }}
{{- if and .Values.serviceMonitor.enabled (not .Values.metrics.service.enabled) }}
{{- fail "serviceMonitor.enabled=true requires metrics.service.enabled=true" }}
{{- end }}
{{- end }}

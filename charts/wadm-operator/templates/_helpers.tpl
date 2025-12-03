{{/*
Expand the name of the chart.
*/}}
{{- define "wadm-operator.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "wadm-operator.fullname" -}}
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
Create chart name and version as used by the chart label.
*/}}
{{- define "wadm-operator.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Namespace that's used for setting up cluster role bindings and such
*/}}
{{- define "wadm-operator.namespace" -}}
{{- default "default" .Release.Namespace }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "wadm-operator.labels" -}}
helm.sh/chart: {{ include "wadm-operator.chart" . }}
{{ include "wadm-operator.selectorLabels" . }}
app.kubernetes.io/component: operator
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: wadm-operator
{{- with .Values.additionalLabels }}
{{ . | toYaml }}
{{- end }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "wadm-operator.selectorLabels" -}}
app.kubernetes.io/name: {{ include "wadm-operator.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "wadm-operator.service-account" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "wadm-operator.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Create the name of the cluster role to use
*/}}
{{- define "wadm-operator.cluster-role" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "wadm-operator.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Create the name of the cluster role binding to use
*/}}
{{- define "wadm-operator.cluster-role-binding" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "wadm-operator.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

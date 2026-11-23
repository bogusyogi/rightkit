package com.orthiclabs.rightkit.android.models

enum class ModelPhase { Missing, Downloading, Verifying, Installing, Ready, Failed }

enum class ModelFailure(val defaultLabel: String) {
    Recovery("Model recovery failed"),
    Installation("Model installation failed"),
}

data class ModelUiState(
    val phase: ModelPhase = ModelPhase.Missing,
    val bytesDone: Long = 0,
    val bytesTotal: Long = 0,
    val error: String? = null,
) {
    val fraction: Float get() = if (bytesTotal > 0) (bytesDone.toFloat() / bytesTotal).coerceIn(0f, 1f) else 0f
}

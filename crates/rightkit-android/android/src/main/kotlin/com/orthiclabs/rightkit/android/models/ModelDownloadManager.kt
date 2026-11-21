package com.orthiclabs.rightkit.android.models

import com.orthiclabs.rightkit.android.core.ModelInstallCancel
import com.orthiclabs.rightkit.android.core.ModelInstallException
import com.orthiclabs.rightkit.android.core.ModelInstallListener
import com.orthiclabs.rightkit.android.core.ModelInstallRequest
import com.orthiclabs.rightkit.android.core.ModelInstallStage
import com.orthiclabs.rightkit.android.core.installModel
import com.orthiclabs.rightkit.android.core.installedModelDir
import com.orthiclabs.rightkit.android.core.recoverModel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

/**
 * Screen state over rightkit-models. Initialise RightKit before use. Native calls block an IO thread.
 * Call close before cancelling the owning scope: scope cancellation alone cannot stop a native call.
 * Cancellation is cooperative; completed activation returns Ready even if cancellation arrived late.
 * onReady runs on an IO thread. App composition and Worker catalogue remain host-owned.
 */
class ModelDownloadManager(
    private val request: ModelInstallRequest,
    private val scope: CoroutineScope,
    private val onReady: ((File) -> Unit)? = null,
    private val failureLabel: (ModelFailure) -> String = { it.defaultLabel },
) : AutoCloseable {
    private val _state = MutableStateFlow(ModelUiState())
    val state: StateFlow<ModelUiState> = _state.asStateFlow()
    private var job: Job? = null
    private var cancelHandle: ModelInstallCancel? = null
    private var recovering = false
    private var closed = false

    /** Startup recovery; failures remain visible and are never converted into Missing. */
    suspend fun recoverAndRefresh() {
        synchronized(this) {
            if (closed || recovering || cancelHandle != null) return
            recovering = true
        }
        try {
            val dir = withContext(Dispatchers.IO) {
                recoverModel(request)
                installedModelDir(request)
            }
            synchronized(this) {
                if (!closed) _state.value = ModelUiState(if (dir == null) ModelPhase.Missing else ModelPhase.Ready)
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            synchronized(this) {
                if (!closed) _state.value = ModelUiState(ModelPhase.Failed, error = failureLabel(ModelFailure.Recovery))
            }
        } finally {
            synchronized(this) { recovering = false }
        }
    }

    @Synchronized
    fun start() {
        // A cancelled Job may still be executing a blocking native call: guard with handle lifetime.
        if (closed || recovering || cancelHandle != null || !scope.isActive) return
        val cancel = ModelInstallCancel()
        cancelHandle = cancel
        _state.value = ModelUiState(ModelPhase.Downloading)
        val next = scope.launch(Dispatchers.IO, start = CoroutineStart.LAZY) {
            val listener = object : ModelInstallListener {
                override fun onProgress(stage: ModelInstallStage, done: ULong, total: ULong) {
                    synchronized(this@ModelDownloadManager) {
                        if (!closed) _state.value = ModelUiState(
                            phase = when (stage) {
                                ModelInstallStage.DOWNLOADING -> ModelPhase.Downloading
                                ModelInstallStage.VERIFYING -> ModelPhase.Verifying
                                ModelInstallStage.INSTALLING -> ModelPhase.Installing
                            },
                            bytesDone = done.coerceAtMost(Long.MAX_VALUE.toULong()).toLong(),
                            bytesTotal = total.coerceAtMost(Long.MAX_VALUE.toULong()).toLong(),
                        )
                    }
                }
            }
            val dir = try {
                installModel(request, cancel, listener)
            } catch (e: ModelInstallException.Cancelled) {
                synchronized(this@ModelDownloadManager) {
                    if (!closed) _state.value = ModelUiState(ModelPhase.Missing)
                }
                return@launch
            } catch (e: Exception) {
                synchronized(this@ModelDownloadManager) {
                    if (!closed) _state.value = ModelUiState(ModelPhase.Failed, error = failureLabel(ModelFailure.Installation))
                }
                return@launch
            }
            synchronized(this@ModelDownloadManager) {
                if (!closed) _state.value = ModelUiState(ModelPhase.Ready)
            }
            if (isActive && synchronized(this@ModelDownloadManager) { !closed }) onReady?.invoke(File(dir))
        }
        job = next
        next.invokeOnCompletion {
            synchronized(this) {
                if (cancelHandle === cancel) {
                    cancelHandle = null
                    job = null
                }
                cancel.destroy()
            }
        }
        next.start()
    }

    /** Requests cooperative cancellation; running native work retains its handle until completion. */
    @Synchronized
    fun cancel() { cancelHandle?.cancel() }

    @Synchronized
    override fun close() {
        closed = true
        cancelHandle?.cancel()
        job?.cancel()
    }
}

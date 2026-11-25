using System;
using System.Threading;
using System.Threading.Tasks;

namespace SoulLink.Desktop.Voice {
    public sealed partial class VoiceBridgeClient {
        public Task<VoiceResult> StartAsync() {
            lock (gate) {
                if (startTask != null) return startTask;
                if (closing) throw VoiceJson.Error("voice_helper_closed");
                startTask = StartCoreAsync(); return startTask;
            }
        }
        async Task<VoiceResult> StartCoreAsync() {
            started = true; HelperGeneration = NextGeneration();
            Exception failure;
            try {
                transport.Start();
                transport.Exit.ContinueWith(task => {
                    if (task.IsFaulted) { var observed = task.Exception; }
                    TransportClosed("voice_helper_exit");
                }, CancellationToken.None, TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
                if (await Task.WhenAny(hello.Task, Task.Delay(helloTimeout)).ConfigureAwait(false) != hello.Task)
                    throw VoiceJson.Error("voice_hello_timeout");
                var result = await hello.Task.ConfigureAwait(false);
                lock (gate) {
                    if (closing || ended) throw VoiceJson.Error("voice_helper_unavailable");
                    ready = true;
                }
                return result;
            } catch (Exception error) { failure = error; }
            // C# 5 does not support await inside a catch block.
            TransportClosed("voice_start_failed");
            try { transport.CloseInput(); } catch { }
            try { transport.Kill(); } catch { }
            await Task.WhenAny(transport.Exit, Task.Delay(killTimeout)).ConfigureAwait(false);
            throw failure;
        }
        public Task<VoiceShutdownResult> ShutdownAsync() {
            lock (gate) {
                if (shutdownTask != null) return shutdownTask;
                closing = true; bindingReady = false;
                if (active != null) active.Revoke();
                shutdownTask = ShutdownCoreAsync(); return shutdownTask;
            }
        }
        async Task<VoiceShutdownResult> ShutdownCoreAsync() {
            var result = new VoiceShutdownResult();
            if (!started) {
                transport.Dispose(); result.CleanupConfirmed = true; return result;
            }
            Task sent;
            lock (gate) {
                if (!ended && sequence < Int64.MaxValue) {
                    string line = json.Serialize(VoiceJson.Object("version", 1, "id", Guid.NewGuid().ToString("N"),
                        "sequence", ++sequence, "op", "shutdown"));
                    try { sent = transport.SendAsync(line); }
                    catch { sent = Task.FromResult(0); TransportClosed("voice_input_unavailable"); }
                } else sent = Task.FromResult(0);
            }
            // Include the write itself in the deadline: a broken/non-reading child cannot hang disposal.
            Task finish = FinishShutdownAsync(sent);
            bool timedOut = await Task.WhenAny(finish, Task.Delay(shutdownTimeout)).ConfigureAwait(false) != finish;
            if (!timedOut) { try { await finish.ConfigureAwait(false); } catch { } }
            if (timedOut || !transport.Exit.IsCompleted) {
                result.ForcedKill = true;
                try { transport.Kill(); } catch { }
                await Task.WhenAny(transport.Exit, Task.Delay(killTimeout)).ConfigureAwait(false);
            }
            if (shutdownReceipt.Task.Status == TaskStatus.RanToCompletion) result.Receipt = shutdownReceipt.Task.Result;
            if (transport.Exit.Status == TaskStatus.RanToCompletion) {
                result.ExitObserved = true; result.ExitCode = transport.Exit.Result;
            }
            result.CleanupConfirmed = !result.ForcedKill && result.ExitObserved && result.ExitCode == 0 && result.Receipt != null
                && Equals(VoiceJson.Get(result.Receipt.Data, "cleanup_confirmed"), true);
            if (!result.CleanupConfirmed) result.Error = timedOut ? "voice_shutdown_timeout" : "voice_cleanup_unconfirmed";
            lock (gate) { RetirePending(result.Error ?? "voice_helper_shutdown", !result.CleanupConfirmed); }
            // Observe faulted completion tasks without exposing diagnostic contents.
            finish.ContinueWith(task => { var observed = task.Exception; }, CancellationToken.None,
                TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            if (transport.Exit.IsFaulted) { var observed = transport.Exit.Exception; }
            if (shutdownReceipt.Task.IsFaulted) { var observed = shutdownReceipt.Task.Exception; }
            if (result.ExitObserved) {
                try { transport.Dispose(); } catch { result.CleanupConfirmed = false; result.Error = "voice_dispose_failed"; }
            }
            return result;
        }
        async Task FinishShutdownAsync(Task sent) {
            await sent.ConfigureAwait(false);
            transport.CloseInput();
            await Task.WhenAll((Task)shutdownReceipt.Task, transport.Exit).ConfigureAwait(false);
        }
        public Task<VoiceShutdownResult> DisposeAsync() { return ShutdownAsync(); }
        public void Dispose() {
            var result = ShutdownAsync().GetAwaiter().GetResult();
            if (!result.CleanupConfirmed) throw VoiceJson.Error(result.Error ?? "voice_cleanup_unconfirmed");
        }
    }
}

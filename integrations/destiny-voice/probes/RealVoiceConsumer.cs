// Explicit local-model probe. Fixed author-written WAV only; never captures a mic.
using System;
using System.Collections.Generic;
using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using System.Web.Script.Serialization;
using SoulLink.Desktop.Voice;

internal static class RealVoiceConsumer {
    static readonly List<object> observations = new List<object>();
    static readonly ConcurrentDictionary<string, VoiceSpeechStarted> starts = new ConcurrentDictionary<string, VoiceSpeechStarted>();
    static void Require(bool ok, string reason) { if (!ok) throw new InvalidOperationException(reason); }
    static string Normalized(string text) { return new string((text ?? "").Where(Char.IsLetterOrDigit).ToArray()).ToLowerInvariant(); }
    static async Task<T> Within<T>(Task<T> work, int seconds, string name) {
        var clock = Stopwatch.StartNew();
        while (!work.IsCompleted && clock.Elapsed.TotalSeconds < seconds) {
            await Task.WhenAny(work, Task.Delay(15000)).ConfigureAwait(false);
            if (!work.IsCompleted) Console.WriteLine("WAIT " + name + " " + (int)clock.Elapsed.TotalSeconds + "s");
        }
        Require(work.IsCompleted, name + "_timeout");
        return await work.ConfigureAwait(false);
    }
    static async Task<VoiceSpeechStarted> WaitStarted(VoiceOperation operation) {
        var clock = Stopwatch.StartNew();
        VoiceSpeechStarted started;
        while (!starts.TryGetValue(operation.Id, out started)) {
            if (operation.Completion.IsCompleted) {
                var early = await operation.Completion.ConfigureAwait(false);
                throw new InvalidOperationException("speech_ended_before_observed_start_" + early.Event);
            }
            Require(clock.Elapsed.TotalSeconds < 60, "speech_start_timeout");
            await Task.Delay(20).ConfigureAwait(false);
        }
        var receipt = (IDictionary<string, object>)started.Data["receipt"];
        Require((string)receipt["engine_kind"] == "qwen_pcm", "unexpected_playback_engine");
        return started;
    }
    static async Task Transcribe(VoiceBridgeClient client, string wav, string expected, string language) {
        var clock = Stopwatch.StartNew();
        var operation = client.Transcribe(File.ReadAllBytes(wav));
        var result = await Within(operation.Completion, 90, "transcribe_" + language).ConfigureAwait(false);
        Require(result.Event == "completed" && client.CanDeliver(operation), "transcription_failed_" + result.Error);
        Require(Normalized(result.Transcript) == Normalized(expected), "transcription_content_mismatch_" + language);
        observations.Add(new { kind = "transcribe", language, elapsed_ms = clock.Elapsed.TotalMilliseconds,
            text = result.Transcript, provider = result.Data["provider"], service_elapsed_ms = result.Data["elapsed_ms"] });
        Console.WriteLine("PASS transcribe_" + language);
    }
    static async Task Speak(VoiceBridgeClient client, string text, string language) {
        var clock = Stopwatch.StartNew();
        var operation = client.Speak(text, language);
        await WaitStarted(operation).ConfigureAwait(false);
        double observedStart = clock.Elapsed.TotalMilliseconds;
        var result = await Within(operation.Completion, 90, "speak_" + language).ConfigureAwait(false);
        Require(result.Event == "completed" && client.CanDeliver(operation), "speech_failed_" + result.Error);
        observations.Add(new { kind = "speak", language, observed_start_ms = observedStart,
            elapsed_ms = clock.Elapsed.TotalMilliseconds, receipt = result.Data, speaker_output_measured = false });
        Console.WriteLine("PASS speak_" + language);
    }
    static async Task<int> Run(string[] args) {
        Require(args.Length == 5 && args[0] == "--real-local-models", "explicit_flag_and_paths_required");
        string reportPath = Path.GetFullPath(args[4]);
        Require(!File.Exists(reportPath), "report_already_exists");
        var client = new VoiceBridgeClient(new VoiceBridgeOptions { ExecutablePath = args[1], VoiceRoot = args[2] });
        client.SpeechStarted += value => starts[value.Operation.Id] = value;
        string failure = null;
        VoiceShutdownResult shutdown = null;
        var overall = Stopwatch.StartNew();
        try {
            await client.StartAsync().ConfigureAwait(false);
            await Within(client.BindAsync("synthetic-csharp-bilingual"), 20, "bind").ConfigureAwait(false);
            var readyClock = Stopwatch.StartNew();
            var preparation = client.Prepare();
            var prepared = await Within(preparation.Completion, 600, "prepare").ConfigureAwait(false);
            Require(prepared.Event == "completed", "prepare_failed_" + prepared.Error);
            observations.Add(new { kind = "prepare", elapsed_ms = readyClock.Elapsed.TotalMilliseconds });
            Console.WriteLine("PASS prepare");
            await Transcribe(client, Path.Combine(args[3], "zh_short.wav"), "先让小车停下来。", "zh").ConfigureAwait(false);
            await Transcribe(client, Path.Combine(args[3], "en_short.wav"), "First, stop the car.", "en").ConfigureAwait(false);
            await Speak(client, "你好，我们一起学习。", "zh").ConfigureAwait(false);
            await Speak(client, "Hello. Let us learn together.", "en").ConfigureAwait(false);
            var longSpeech = client.Speak(String.Concat(Enumerable.Repeat("Let us compare two measurements carefully. ", 16)), "en");
            await WaitStarted(longSpeech).ConfigureAwait(false);
            var cancelClock = Stopwatch.StartNew();
            var cancellation = client.CancelAsync(longSpeech);
            Require(longSpeech.DeliveryRevoked && !client.CanDeliver(longSpeech), "cancel_did_not_fence_delivery");
            var canceledRequest = await Within(cancellation, 10, "cancel_request").ConfigureAwait(false);
            Require(canceledRequest.Event == "cancel_requested", "cancel_request_failed");
            var canceled = await Within(longSpeech.Completion, 15, "cancel_terminal").ConfigureAwait(false);
            Require(canceled.Event == "canceled" && canceled.StopError == null && !client.Busy, "cancel_not_confirmed");
            observations.Add(new { kind = "cancel", elapsed_to_terminal_ms = cancelClock.Elapsed.TotalMilliseconds,
                delivery_revoked = canceled.DeliveryRevoked, stop_error = canceled.StopError, speaker_output_measured = false });
            Console.WriteLine("PASS cancel");
        } catch (Exception error) {
            // Only our fixed test codes are retained. No transport/process text is emitted.
            failure = error is InvalidOperationException ? error.Message : error.GetType().Name;
            Console.WriteLine("FAIL consumer");
        }
        try { shutdown = await client.ShutdownAsync().ConfigureAwait(false); }
        catch { failure = failure ?? "shutdown_exception"; }
        try { client.Dispose(); } catch { failure = failure ?? "dispose_unconfirmed"; }
        bool passed = failure == null && shutdown != null && shutdown.CleanupConfirmed && shutdown.ExitObserved && shutdown.ExitCode == 0 && !shutdown.ForcedKill;
        var report = new { passed, failure, elapsed_ms = overall.Elapsed.TotalMilliseconds, observations,
            shutdown, physical_microphone = false, speaker_output_measured = false, cloud_calls = 0,
            destiny_window_used = false, synthetic_only = true };
        using (var stream = new FileStream(reportPath, FileMode.CreateNew, FileAccess.Write))
        using (var writer = new StreamWriter(stream, new UTF8Encoding(false)))
            writer.Write(new JavaScriptSerializer { MaxJsonLength = 2000000 }.Serialize(report));
        Console.WriteLine(passed ? "PASS owned_shutdown" : "FAIL result");
        return passed ? 0 : 1;
    }
    static int Main(string[] args) { try { return Run(args).GetAwaiter().GetResult(); } catch { Console.WriteLine("FAIL probe_setup"); return 1; } }
}

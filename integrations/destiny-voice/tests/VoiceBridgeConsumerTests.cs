using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using System.Web.Script.Serialization;
using SoulLink.Desktop.Voice;

internal sealed class FakeTransport : IVoiceBridgeTransport
{
    internal readonly object Gate = new object();
    internal readonly List<Dictionary<string, object>> Requests = new List<Dictionary<string, object>>();
    internal readonly TaskCompletionSource<int> ExitSource = new TaskCompletionSource<int>();
    internal bool EmitHello = true, ShutdownReceipt = true, ShutdownExit = true, Cleanup = true;
    internal bool IgnoreClose = false, Killed = false;
    internal int WriteDelayMs = 0, WritesInProgress = 0, MaxWritesInProgress = 0;
    public event Action<string> LineReceived;
    public event Action<string> Closed;
    public Task<int> Exit { get { return ExitSource.Task; } }
    public long StderrCharacterCount { get { return 0; } }
    public void Start() { if (EmitHello) EmitGlobal("hello", Obj("automatic_prepare", false, "transport", "stdio_jsonl_v1")); }
    public Task SendAsync(string line)
    {
        int writes = Interlocked.Increment(ref WritesInProgress);
        lock (Gate) { MaxWritesInProgress = Math.Max(MaxWritesInProgress, writes); }
        try
        {
            if (WriteDelayMs > 0) Thread.Sleep(WriteDelayMs);
            Dictionary<string, object> request = new JavaScriptSerializer().Deserialize<Dictionary<string, object>>(line);
            lock (Gate) { Requests.Add(request); }
            string op = Convert.ToString(request["op"]);
            if (op == "bind") Reply(request, "bound", Obj("scope", request["scope"]));
            else if (op == "status") Reply(request, "status", Obj("backend", Obj("ready", false)));
            else if (op == "cancel") Reply(request, "cancel_requested", Obj("operation_id", request["operation_id"]));
            else if (op == "shutdown")
            {
                if (ShutdownReceipt) EmitGlobal("shutdown", Obj("cleanup_confirmed", Cleanup));
                if (ShutdownExit) End("voice_transport_eof", 0);
            }
            return Done();
        }
        finally { Interlocked.Decrement(ref WritesInProgress); }
    }
    public void CloseInput() { if (!IgnoreClose && !Exit.IsCompleted) End("voice_transport_eof", 0); }
    public void Kill() { Killed = true; End("voice_transport_exit", -1); }
    public void Dispose() { }
    internal void End(string code, int exit)
    {
        ExitSource.TrySetResult(exit);
        var handler = Closed;
        if (handler != null) handler(code);
    }
    internal Dictionary<string, object> Request(string op)
    {
        lock (Gate) { return Requests.LastOrDefault(item => Convert.ToString(item["op"]) == op); }
    }
    internal int Count(string op) { lock (Gate) { return Requests.Count(item => Convert.ToString(item["op"]) == op); } }
    internal void Reply(VoiceOperation operation, string name, Dictionary<string, object> data)
    { Reply(Obj("id", operation.Id, "sequence", operation.Sequence), name, data); }
    internal void Reply(Dictionary<string, object> request, string name, Dictionary<string, object> data)
    { Emit(Obj("version", 1, "id", request["id"], "sequence", request["sequence"], "event", name, "data", data)); }
    internal void EmitGlobal(string name, Dictionary<string, object> data) { Emit(Obj("version", 1, "event", name, "data", data)); }
    internal void Emit(Dictionary<string, object> value)
    {
        var handler = LineReceived;
        if (handler != null) handler(new JavaScriptSerializer().Serialize(value));
    }
    internal static Task Done() { var source = new TaskCompletionSource<bool>(); source.SetResult(true); return source.Task; }
    internal static Dictionary<string, object> Obj(params object[] pairs)
    {
        var value = new Dictionary<string, object>();
        for (int i = 0; i < pairs.Length; i += 2) value.Add((string)pairs[i], pairs[i + 1]);
        return value;
    }
}

internal sealed class Fixture : IDisposable
{
    internal readonly FakeTransport Transport;
    internal readonly VoiceBridgeClient Client;
    internal Fixture(bool bind = true, FakeTransport transport = null)
    {
        Transport = transport ?? new FakeTransport();
        Client = new VoiceBridgeClient(VoiceBridgeConsumerTests.Options("test-owned-helper", "test-owned-root"), Transport);
        VoiceBridgeConsumerTests.Await(Client.StartAsync());
        if (bind) VoiceBridgeConsumerTests.Await(Client.BindAsync("owner-a"));
    }
    public void Dispose() { VoiceBridgeConsumerTests.Await(Client.DisposeAsync(), 3000); }
}

internal static class VoiceBridgeConsumerTests
{
    private static int Failed;
    private static string Bridge, Root, Helper;
    internal static VoiceBridgeOptions Options(string executable, string root)
    {
        return new VoiceBridgeOptions { ExecutablePath = executable, VoiceRoot = root,
            HelloTimeout = TimeSpan.FromMilliseconds(350), ShutdownTimeout = TimeSpan.FromMilliseconds(450),
            KillTimeout = TimeSpan.FromMilliseconds(350) };
    }
    internal static T Await<T>(Task<T> task, int milliseconds = 2500)
    {
        if (!task.Wait(milliseconds)) throw new Exception("task_did_not_settle");
        return task.Result;
    }
    private static void Check(bool truth, string code) { if (!truth) throw new Exception(code); }
    private static void Until(Func<bool> test, string code, int milliseconds = 1500)
    {
        Stopwatch watch = Stopwatch.StartNew();
        while (!test() && watch.ElapsedMilliseconds < milliseconds) Thread.Sleep(5);
        Check(test(), code);
    }
    private static Dictionary<string, object> Obj(params object[] pairs) { return FakeTransport.Obj(pairs); }
    private static object Value(object data, string key)
    {
        var map = data as IDictionary<string, object>;
        return map != null && map.ContainsKey(key) ? map[key] : null;
    }
    private static Dictionary<string, object> Scope(VoiceOperation operation)
    { return Obj("owner", operation.Scope.Owner, "generation", operation.Scope.Generation); }
    private static Dictionary<string, object> Started()
    { return Obj("chunk_index", 2, "receipt", Obj("playback_id", "test-playback-7", "epoch", 77, "engine_kind", "windows_sapi", "milestone", "first_pcm_submitted")); }
    private static void Finish(Fixture fixture, VoiceOperation operation)
    { fixture.Transport.Reply(operation, "completed", Obj("scope", Scope(operation), "speaker_output_measured", false)); Await(operation.Completion); }
    private static void Test(string name, Action body)
    {
        try { body(); Console.WriteLine("PASS\t" + name); }
        catch (Exception error)
        {
            Failed++;
            while (error is AggregateException && error.InnerException != null) error = error.InnerException;
            // Messages originate in test assertions or safe adapter error codes, never protocol dumps.
            string detail = error.Message;
            if (detail.Length > 120 || detail.Any(c => !(Char.IsLetterOrDigit(c) || c == '_' || c == ' ' || c == ':' || c == '-')))
                detail = error.GetType().Name;
            Console.WriteLine("FAIL\t" + name + "\t" + detail);
        }
    }
    public static int Main(string[] args)
    {
        Bridge = args[0]; Root = args[1]; Helper = args[2];
        Test("accepted_is_not_terminal_or_started", AcceptedIsNotTerminal);
        Test("speech_started_preserves_receipt_and_correlation", StartedReceipt);
        Test("cancel_fences_targets_exact_identity_and_waits_terminal", CancelRetirement);
        Test("cancel_stop_error_remains_visible", CancelStopError);
        Test("owner_switch_fences_old_starts_and_transcripts", OwnerSwitch);
        Test("release_invalidates_binding_until_fresh_bind", ReleaseBinding);
        Test("wav_20_second_limit_rejects_without_truncation", WaveLimit);
        Test("text_limit_counts_utf16_and_preserves_boundary", TextLimit);
        Test("prepare_survives_three_second_nonterminal_wait", LongPrepare);
        Test("eof_settles_pending_operation", EofSettles);
        Test("hello_deadline_is_bounded", HelloDeadline);
        Test("shutdown_missing_receipt_is_unconfirmed", ShutdownMissingReceipt);
        Test("shutdown_missing_exit_is_unconfirmed", ShutdownMissingExit);
        Test("shutdown_negative_cleanup_is_unconfirmed", ShutdownNegativeReceipt);
        Test("concurrent_writes_are_serial_and_increasing", ConcurrentWrites);
        Test("process_transport_consumes_stderr_without_body_delivery", ProcessStderr);
        Test("process_transport_eof_settles_pending", ProcessEof);
        Test("rust_release_model_free_hello_bind_status_media_shutdown", RealRust);
        return Failed == 0 ? 0 : 1;
    }
    private static void AcceptedIsNotTerminal()
    {
        using (var f = new Fixture())
        {
            int starts = 0; f.Client.SpeechStarted += ignored => Interlocked.Increment(ref starts);
            var operation = f.Client.Speak("synthetic test sentence", "en");
            f.Transport.Reply(operation, "accepted", Obj("scope", Scope(operation)));
            Check(!operation.Completion.IsCompleted && f.Client.Busy && starts == 0, "accepted_was_treated_as_terminal_or_start");
            Finish(f, operation);
        }
    }
    private static void StartedReceipt()
    {
        using (var f = new Fixture())
        {
            int starts = 0; VoiceSpeechStarted receipt = null;
            f.Client.SpeechStarted += value => { receipt = value; Interlocked.Increment(ref starts); };
            var operation = f.Client.Speak("synthetic test sentence", "en");
            f.Transport.Reply(Obj("id", operation.Id, "sequence", operation.Sequence + 1), "speech_started", Started());
            Check(starts == 0, "mismatched_sequence_start_delivered");
            f.Transport.Reply(Obj("id", "other-operation", "sequence", operation.Sequence), "speech_started", Started());
            Check(starts == 0, "mismatched_id_start_delivered");
            f.Transport.Reply(operation, "speech_started", Started());
            Until(() => starts == 1, "start_not_delivered");
            Check(receipt.Operation == operation, "start_operation_identity_lost");
            object actual = Value(receipt.Data, "receipt");
            Check(Convert.ToString(Value(actual, "engine_kind")) == "windows_sapi" && Convert.ToString(Value(actual, "playback_id")) == "test-playback-7" && Convert.ToInt64(Value(actual, "epoch")) == 77, "actual_playback_receipt_lost");
            Check(!operation.Completion.IsCompleted, "start_was_treated_as_terminal");
            Finish(f, operation);
        }
    }
    private static void CancelRetirement()
    {
        using (var f = new Fixture())
        {
            int starts = 0; f.Client.SpeechStarted += ignored => Interlocked.Increment(ref starts);
            var operation = f.Client.Speak("synthetic test sentence", "en");
            var cancel = f.Client.CancelAsync(operation);
            Check(operation.DeliveryRevoked && !f.Client.CanDeliver(operation), "cancel_did_not_fence_synchronously");
            Until(() => f.Transport.Request("cancel") != null, "cancel_not_written");
            var request = f.Transport.Request("cancel");
            Check(Convert.ToString(request["operation_id"]) == operation.Id && Convert.ToInt64(request["operation_sequence"]) == operation.Sequence, "cancel_target_identity_inexact");
            Check(f.Client.Busy && !operation.Completion.IsCompleted, "cancel_ack_retired_operation");
            bool denied = false;
            try { f.Client.Speak("synthetic replacement", "en"); } catch (InvalidOperationException) { denied = true; }
            Check(denied && f.Transport.Count("speak") == 1, "replacement_started_before_terminal");
            f.Transport.Reply(operation, "speech_started", Started());
            Check(starts == 0, "canceled_start_delivered");
            f.Transport.Reply(operation, "canceled", Obj("scope", Scope(operation), "delivery_revoked", true, "stop_error", null));
            VoiceResult terminal = Await(operation.Completion); Await(cancel);
            Check(terminal.DeliveryRevoked && !f.Client.Busy, "canceled_terminal_not_retired");
            var replacement = f.Client.Speak("synthetic replacement", "en"); Finish(f, replacement);
        }
    }
    private static void CancelStopError()
    {
        using (var f = new Fixture())
        {
            var operation = f.Client.Speak("synthetic test sentence", "en"); f.Client.CancelAsync(operation);
            f.Transport.Reply(operation, "canceled", Obj("delivery_revoked", true, "stop_error", "voice_stop_unconfirmed"));
            var result = Await(operation.Completion);
            Check(result.StopError == "voice_stop_unconfirmed" && result.DeliveryRevoked, "stop_error_was_hidden");
        }
    }
    private static void OwnerSwitch()
    {
        using (var f = new Fixture())
        {
            int starts = 0; f.Client.SpeechStarted += ignored => Interlocked.Increment(ref starts);
            var oldSpeech = f.Client.Speak("synthetic old owner", "en");
            var binding = f.Client.BindAsync("owner-b");
            Check(oldSpeech.DeliveryRevoked && !f.Client.CanDeliver(oldSpeech), "owner_switch_did_not_fence_synchronously");
            var scope = Await(binding);
            Check(scope.Owner == "owner-b" && scope.Generation > oldSpeech.Scope.Generation, "owner_switch_generation_not_increased");
            f.Transport.Reply(oldSpeech, "speech_started", Started());
            f.Transport.Reply(oldSpeech, "completed", Obj("scope", Scope(oldSpeech)));
            Check(Await(oldSpeech.Completion).DeliveryRevoked && starts == 0, "stale_owner_start_delivered");
            var old = f.Client.Transcribe(Wave());
            Await(f.Client.BindAsync("owner-c"));
            f.Transport.Reply(old, "completed", Obj("scope", Scope(old), "text", "PRIVATE_TRANSCRIPT_SENTINEL", "final", true));
            var result = Await(old.Completion);
            Check(starts == 0 && result.DeliveryRevoked && String.IsNullOrEmpty(result.Transcript), "stale_owner_result_delivered");
            var current = f.Client.Transcribe(Wave());
            f.Transport.Reply(current, "completed", Obj("scope", Scope(current), "text", "synthetic current transcript", "final", true));
            Check(Await(current.Completion).Transcript == "synthetic current transcript", "current_owner_transcript_missing");
        }
    }
    private static void LongPrepare()
    {
        using (var f = new Fixture(false))
        {
            var operation = f.Client.Prepare(); f.Transport.Reply(operation, "accepted", Obj("scope", null));
            Thread.Sleep(3250);
            Check(!operation.Completion.IsCompleted && f.Client.Busy, "prepare_used_three_second_command_timeout");
            f.Transport.Reply(operation, "completed", Obj("ready", true));
            Check(Await(operation.Completion).Event == "completed", "prepare_terminal_missing");
        }
    }
    private static void ReleaseBinding()
    {
        using (var f = new Fixture())
        {
            var operation = f.Client.Transcribe(Wave());
            var invalidation = f.Client.InvalidateBindingAsync();
            Check(operation.DeliveryRevoked && !f.Client.CanDeliver(operation), "release_did_not_fence_synchronously");
            f.Transport.Reply(operation, "canceled", Obj("delivery_revoked", true, "stop_error", null));
            Await(operation.Completion); Await(invalidation);
            bool unbound = false;
            try { f.Client.Speak("synthetic unbound", "en"); } catch (InvalidOperationException) { unbound = true; }
            Check(unbound, "release_allowed_media_without_fresh_bind");
            var fresh = Await(f.Client.BindAsync("owner-a"));
            Check(fresh.Generation > operation.Scope.Generation, "release_reused_binding_generation");
            Finish(f, f.Client.Speak("synthetic rebound", "en"));
        }
    }
    private static void WaveLimit()
    {
        using (var f = new Fixture())
        {
            bool rejected = false;
            try { f.Client.Transcribe(Wave(640002)); } catch (ArgumentException) { rejected = true; }
            Check(rejected && f.Transport.Count("transcribe") == 0, "oversized_wave_sent_or_truncated");
            byte[] valid = Wave(640000);
            var operation = f.Client.Transcribe(valid);
            byte[] sent = Convert.FromBase64String(Convert.ToString(f.Transport.Request("transcribe")["wav_base64"]));
            Check(sent.Length == valid.Length && sent.SequenceEqual(valid), "twenty_second_wave_was_truncated_or_changed");
            Finish(f, operation);
            byte[] stereo = Wave(); stereo[22] = 2; rejected = false;
            try { f.Client.Transcribe(stereo); } catch (ArgumentException) { rejected = true; }
            Check(rejected && f.Transport.Count("transcribe") == 1, "unsupported_wave_format_sent");
        }
    }
    private static void TextLimit()
    {
        using (var f = new Fixture())
        {
            string boundary = String.Concat(Enumerable.Repeat("\ud83d\ude00", 1000));
            bool rejected = false;
            try { f.Client.Speak(boundary + "x", "en"); } catch (ArgumentException) { rejected = true; }
            Check(rejected && f.Transport.Count("speak") == 0, "utf16_oversize_text_sent");
            var operation = f.Client.Speak(boundary, "en");
            Check(Convert.ToString(f.Transport.Request("speak")["text"]) == boundary, "utf16_boundary_text_changed");
            Finish(f, operation);
        }
    }
    private static void EofSettles()
    {
        using (var f = new Fixture())
        {
            var operation = f.Client.Transcribe(Wave()); f.Transport.End("voice_transport_eof", 0);
            var result = Await(operation.Completion);
            Check(result.DeliveryRevoked && !String.IsNullOrEmpty(result.Error) && String.IsNullOrEmpty(result.Transcript), "eof_pending_operation_not_revoked");
        }
    }
    private static void HelloDeadline()
    {
        var transport = new FakeTransport { EmitHello = false, IgnoreClose = true };
        var client = new VoiceBridgeClient(Options("test-owned-helper", "test-owned-root"), transport);
        Stopwatch clock = Stopwatch.StartNew(); bool failed = false;
        try { var result = Await(client.StartAsync()); failed = !String.IsNullOrEmpty(result.Error); } catch (Exception) { failed = true; }
        Check(failed && clock.ElapsedMilliseconds < 1800, "hello_timeout_not_bounded");
        var shutdown = Await(client.DisposeAsync());
        Check(!shutdown.CleanupConfirmed, "missing_hello_reported_confirmed_cleanup");
    }
    private static void ShutdownMissingReceipt()
    {
        using (var f = new Fixture(true, new FakeTransport { ShutdownReceipt = false }))
        {
            var result = Await(f.Client.ShutdownAsync());
            Check(!result.CleanupConfirmed && result.ExitObserved, "exit_without_receipt_claimed_cleanup");
        }
    }
    private static void ShutdownMissingExit()
    {
        using (var f = new Fixture(true, new FakeTransport { ShutdownExit = false, IgnoreClose = true }))
        {
            Stopwatch clock = Stopwatch.StartNew(); var result = Await(f.Client.ShutdownAsync());
            Check(!result.CleanupConfirmed && result.ForcedKill && f.Transport.Killed && clock.ElapsedMilliseconds < 1800, "receipt_without_exit_claimed_cleanup_or_hung");
        }
    }
    private static void ShutdownNegativeReceipt()
    {
        using (var f = new Fixture(true, new FakeTransport { Cleanup = false }))
        {
            var result = Await(f.Client.ShutdownAsync());
            Check(!result.CleanupConfirmed && result.ExitObserved && !String.IsNullOrEmpty(result.Error), "negative_cleanup_receipt_hidden");
        }
    }
    private static void ConcurrentWrites()
    {
        using (var f = new Fixture())
        {
            f.Transport.WriteDelayMs = 3;
            var tasks = Enumerable.Range(0, 24).Select(ignored => Task.Factory.StartNew(() => f.Client.StatusAsync()).Unwrap()).ToArray();
            Check(Task.WaitAll(tasks, 10000), "concurrent_status_did_not_settle");
            long previous = 0; var ids = new HashSet<string>();
            lock (f.Transport.Gate)
            {
                foreach (var item in f.Transport.Requests)
                {
                    long sequence = Convert.ToInt64(item["sequence"]);
                    Check(sequence > previous, "wire_sequence_reordered"); previous = sequence;
                    Check(ids.Add(Convert.ToString(item["id"])), "request_id_reused");
                }
                Check(f.Transport.MaxWritesInProgress == 1, "concurrent_transport_write_overlap");
            }
        }
    }
    private static void ProcessStderr()
    {
        var options = Options(Helper, Root); options.HelloTimeout = TimeSpan.FromSeconds(4); options.ShutdownTimeout = TimeSpan.FromSeconds(3);
        var transport = new OwnedVoiceProcess(options.ExecutablePath, options.VoiceRoot);
        var client = new VoiceBridgeClient(options, transport);
        var hello = Await(client.StartAsync(), 5000); Await(client.BindAsync("synthetic-owner")); Await(client.StatusAsync());
        Until(() => transport.StderrCharacterCount >= 131072, "stderr_not_continuously_drained");
        Check(!new JavaScriptSerializer().Serialize(hello.Data).Contains("PRIVATE_STDERR_SENTINEL"), "stderr_body_exposed_as_hello");
        var result = Await(client.DisposeAsync(), 5000);
        Check(result.CleanupConfirmed && result.ExitObserved && result.ExitCode == 0 && !result.ForcedKill, "synthetic_process_cleanup_unconfirmed");
        Check(String.IsNullOrEmpty(result.Error) || !result.Error.Contains("PRIVATE_STDERR_SENTINEL"), "stderr_body_exposed_as_error");
    }
    private static void ProcessEof()
    {
        string syntheticRoot = Path.Combine(Path.GetDirectoryName(Root), "synthetic-eof"); Directory.CreateDirectory(syntheticRoot);
        var options = Options(Helper, syntheticRoot); options.HelloTimeout = TimeSpan.FromSeconds(4);
        var client = new VoiceBridgeClient(options); Await(client.StartAsync(), 5000); Await(client.BindAsync("synthetic-owner"));
        var operation = client.Speak("synthetic eof test", "en"); var result = Await(operation.Completion, 5000);
        Check(result.DeliveryRevoked && !String.IsNullOrEmpty(result.Error), "process_eof_did_not_settle_pending");
        Check(!Await(client.DisposeAsync(), 5000).CleanupConfirmed, "eof_without_cleanup_receipt_claimed_confirmed");
    }
    private static void RealRust()
    {
        var options = Options(Bridge, Root); options.HelloTimeout = TimeSpan.FromSeconds(5); options.ShutdownTimeout = TimeSpan.FromSeconds(8);
        var client = new VoiceBridgeClient(options);
        try
        {
            var hello = Await(client.StartAsync(), 7000);
            Check(hello.Event == "hello" && Object.Equals(Value(hello.Data, "automatic_prepare"), false), "rust_hello_not_model_free");
            Check(Convert.ToString(Value(Value(hello.Data, "asr"), "capture_owner")) == "caller", "rust_capture_ownership_changed");
            var bound = Await(client.BindAsync("consumer-synthetic-owner")); Check(bound.Generation > 0, "rust_bind_failed");
            var status = Await(client.StatusAsync()); var backend = Value(status.Data, "backend");
            Check(Object.Equals(Value(backend, "ready"), false) && Convert.ToString(Value(backend, "phase")) == "idle", "rust_started_models_without_prepare");
            Check(Value(status.Data, "audio_processing") == null, "rust_created_audio_owner_without_prepare");
            var speech = client.Speak("synthetic model-free test", "en"); var speechResult = Await(speech.Completion, 5000);
            Check(speechResult.Event == "failed" && !String.IsNullOrEmpty(speechResult.Error), "rust_unprepared_speech_not_rejected");
            var transcript = client.Transcribe(Wave()); var asrResult = Await(transcript.Completion, 5000);
            Check(asrResult.Event == "failed" && !String.IsNullOrEmpty(asrResult.Error) && String.IsNullOrEmpty(asrResult.Transcript), "rust_unprepared_asr_not_rejected");
            status = Await(client.StatusAsync()); backend = Value(status.Data, "backend");
            Check(Object.Equals(Value(backend, "ready"), false) && Value(status.Data, "audio_processing") == null, "rust_media_created_resources_without_prepare");
            var shutdown = Await(client.ShutdownAsync(), 10000);
            Check(shutdown.CleanupConfirmed && shutdown.ExitObserved && shutdown.ExitCode == 0 && !shutdown.ForcedKill, "rust_shutdown_cleanup_exit_unconfirmed");
        }
        finally { Await(client.DisposeAsync(), 10000); }
    }
    private static byte[] Wave(int pcmBytes = 2)
    {
        using (var stream = new MemoryStream()) using (var writer = new BinaryWriter(stream))
        {
            writer.Write(System.Text.Encoding.ASCII.GetBytes("RIFF")); writer.Write(36 + pcmBytes);
            writer.Write(System.Text.Encoding.ASCII.GetBytes("WAVEfmt ")); writer.Write(16); writer.Write((short)1);
            writer.Write((short)1); writer.Write(16000); writer.Write(32000); writer.Write((short)2); writer.Write((short)16);
            writer.Write(System.Text.Encoding.ASCII.GetBytes("data")); writer.Write(pcmBytes); writer.Write(new byte[pcmBytes]);
            writer.Flush(); return stream.ToArray();
        }
    }
}

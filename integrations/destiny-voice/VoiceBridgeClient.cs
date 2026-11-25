using System;
using System.Collections.Generic;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Web.Script.Serialization;

namespace SoulLink.Desktop.Voice {
    public sealed partial class VoiceBridgeClient : IDisposable {
        static long generations;
        readonly object gate = new object();
        readonly IVoiceBridgeTransport transport;
        readonly TimeSpan helloTimeout, shutdownTimeout, killTimeout;
        readonly JavaScriptSerializer json = new JavaScriptSerializer { MaxJsonLength = 1500000, RecursionLimit = 32 };
        readonly Dictionary<string, Pending> pending = new Dictionary<string, Pending>();
        readonly TaskCompletionSource<VoiceResult> hello = new TaskCompletionSource<VoiceResult>();
        readonly TaskCompletionSource<VoiceResult> shutdownReceipt = new TaskCompletionSource<VoiceResult>();
        VoiceScope scope;
        VoiceOperation active;
        bool started, ready, bindingReady, closing, ended;
        long sequence;
        Task<VoiceResult> startTask;
        Task<VoiceShutdownResult> shutdownTask;
        public long HelperGeneration { get; private set; }
        public bool Busy { get { lock (gate) return active != null; } }
        public long StderrCharacterCount { get { return transport.StderrCharacterCount; } }
        // Runs on the reader thread. Marshal to the host UI and recheck CanDeliver there.
        public event Action<VoiceSpeechStarted> SpeechStarted;

        sealed class Pending {
            internal string Id, Kind;
            internal long Sequence;
            internal VoiceOperation Operation;
            internal TaskCompletionSource<VoiceResult> Source;
        }
        public VoiceBridgeClient(VoiceBridgeOptions options)
            : this(options, new OwnedVoiceProcess(options.ExecutablePath, options.VoiceRoot)) { }
        public VoiceBridgeClient(VoiceBridgeOptions options, IVoiceBridgeTransport transport) {
            if (options == null || transport == null) throw new ArgumentNullException();
            ValidateTimeout(options.HelloTimeout); ValidateTimeout(options.ShutdownTimeout); ValidateTimeout(options.KillTimeout);
            helloTimeout = options.HelloTimeout; shutdownTimeout = options.ShutdownTimeout; killTimeout = options.KillTimeout;
            this.transport = transport;
            transport.LineReceived += Receive; transport.Closed += TransportClosed;
        }
        static void ValidateTimeout(TimeSpan value) {
            if (value.TotalMilliseconds < 1 || value.TotalMilliseconds > Int32.MaxValue) throw new ArgumentOutOfRangeException("timeout");
        }
        static long NextGeneration() {
            long value = Interlocked.Increment(ref generations);
            if (value <= 0) throw VoiceJson.Error("voice_generation_exhausted");
            return value;
        }
        public bool CanDeliver(VoiceOperation operation) {
            lock (gate) return CanDeliverLocked(operation);
        }
        bool CanDeliverLocked(VoiceOperation operation) {
            return operation != null && operation.Owner == this && !operation.DeliveryRevoked && !closing && !ended
                && operation.HelperGeneration == HelperGeneration
                && (operation.Scope == null || ReferenceEquals(operation.Scope, scope));
        }
        void RequireStarted() {
            if (!ready || closing || ended) throw VoiceJson.Error("voice_helper_unavailable");
        }
        Pending NewRequest(string kind, Dictionary<string, object> fields, VoiceScope mediaScope, bool operation) {
            if (sequence == Int64.MaxValue) throw VoiceJson.Error("voice_sequence_exhausted");
            var request = new Pending { Id = Guid.NewGuid().ToString("N"), Sequence = ++sequence, Kind = kind };
            if (operation) {
                request.Operation = new VoiceOperation(this, request.Id, request.Sequence, kind, mediaScope, HelperGeneration);
                request.Source = request.Operation.Source; active = request.Operation;
            } else request.Source = new TaskCompletionSource<VoiceResult>();
            pending.Add(request.Id, request);
            fields.Add("version", 1); fields.Add("id", request.Id); fields.Add("sequence", request.Sequence); fields.Add("op", kind);
            Send(fields);
            return request;
        }
        void Send(Dictionary<string, object> request) {
            try {
                string line = json.Serialize(request);
                if (Encoding.UTF8.GetByteCount(line) + 1 > 1500000) throw VoiceJson.Error("voice_input_frame_limit");
                transport.SendAsync(line).ContinueWith(task => {
                    if (task.IsFaulted) { var observed = task.Exception; TransportClosed("voice_input_unavailable"); }
                    else if (task.IsCanceled) TransportClosed("voice_input_unavailable");
                }, CancellationToken.None, TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            } catch { TransportClosed("voice_input_unavailable"); }
        }
        void Receive(string line) {
            VoiceSpeechStarted speech = null;
            try {
                lock (gate) {
                    var envelope = json.DeserializeObject(line) as Dictionary<string, object>;
                    if (envelope == null || VoiceJson.Integer(envelope, "version") != 1) throw VoiceJson.Error("voice_protocol_invalid");
                    string kind = VoiceJson.Text(envelope, "event");
                    var data = VoiceJson.Get(envelope, "data") as Dictionary<string, object>;
                    if (kind == null || data == null) throw VoiceJson.Error("voice_protocol_invalid");
                    if (kind == "hello" && VoiceJson.Get(envelope, "id") == null) {
                        if (hello.Task.IsCompleted) throw VoiceJson.Error("voice_protocol_invalid");
                        hello.TrySetResult(new VoiceResult(kind, null, 0, data, false)); return;
                    }
                    if (kind == "shutdown" && VoiceJson.Get(envelope, "id") == null) {
                        shutdownReceipt.TrySetResult(new VoiceResult(kind, null, 0, data, false));
                        closing = true; RetirePending("voice_helper_shutdown", !Equals(VoiceJson.Get(data, "cleanup_confirmed"), true)); return;
                    }
                    string id = VoiceJson.Text(envelope, "id");
                    if (id == null) throw VoiceJson.Error("voice_protocol_uncorrelated");
                    long receivedSequence = VoiceJson.Integer(envelope, "sequence");
                    Pending request;
                    if (!pending.TryGetValue(id, out request) || request.Sequence != receivedSequence) return;
                    if (kind == "accepted") {
                        if (request.Operation == null) throw VoiceJson.Error("voice_protocol_invalid");
                        return;
                    }
                    if (kind == "speech_started") {
                        if (request.Kind != "speak") throw VoiceJson.Error("voice_protocol_invalid");
                        if (CanDeliverLocked(request.Operation)) speech = new VoiceSpeechStarted(request.Operation, data);
                    } else {
                        bool terminal = kind == "completed" || kind == "failed" || kind == "canceled" || kind == "rejected";
                        bool control = (request.Kind == "bind" && kind == "bound") || (request.Kind == "status" && kind == "status")
                            || (request.Kind == "cancel" && kind == "cancel_requested");
                        if (!(request.Operation != null ? terminal : (control || kind == "rejected"))) throw VoiceJson.Error("voice_protocol_invalid");
                        pending.Remove(id);
                        if (request.Operation == active) active = null;
                        Complete(request, kind, data);
                    }
                }
            } catch { TransportClosed("voice_protocol_invalid"); return; }
            if (speech != null && CanDeliver(speech.Operation)) {
                var callback = SpeechStarted;
                // A host callback failure must not strand the pipe or operation.
                if (callback != null) { try { callback(speech); } catch { } }
            }
        }
        void Complete(Pending request, string kind, IDictionary<string, object> data) {
            // .NET 4.5 lacks RunContinuationsAsynchronously: isolate caller continuations.
            ThreadPool.QueueUserWorkItem(delegate {
                VoiceResult result;
                lock (gate) {
                    bool revoked = request.Operation != null && !CanDeliverLocked(request.Operation);
                    if (kind == "canceled" && request.Operation != null) { request.Operation.Revoke(); revoked = true; }
                    var delivered = revoked ? VoiceJson.Object("delivery_revoked", true,
                        "error", VoiceJson.Get(data, "error"), "stop_error", VoiceJson.Get(data, "stop_error")) : data;
                    result = new VoiceResult(kind, request.Id, request.Sequence, delivered, revoked);
                }
                request.Source.TrySetResult(result);
            });
        }
        void RetirePending(string error, bool stopUnconfirmed) {
            foreach (var request in pending.Values) {
                if (request.Operation != null) request.Operation.Revoke();
                Complete(request, "failed", VoiceJson.Object("error", error, "stop_error", stopUnconfirmed ? "voice_stop_unconfirmed" : null));
            }
            pending.Clear(); active = null;
        }
        void TransportClosed(string reason) {
            lock (gate) {
                if (ended) return; ended = true; ready = false; bindingReady = false;
                // Never propagate arbitrary transport diagnostic content.
                string code = reason == "voice_helper_eof" ? reason : "voice_transport_failed";
                hello.TrySetException(VoiceJson.Error(code)); shutdownReceipt.TrySetException(VoiceJson.Error(code));
                RetirePending(code, true);
            }
        }
    }
}

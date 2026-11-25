using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Threading;
using System.Threading.Tasks;

namespace SoulLink.Desktop.Voice {
    public sealed class VoiceBridgeOptions {
        public string ExecutablePath { get; set; }
        public string VoiceRoot { get; set; }
        public TimeSpan HelloTimeout { get; set; }
        public TimeSpan ShutdownTimeout { get; set; }
        public TimeSpan KillTimeout { get; set; }
        public VoiceBridgeOptions() {
            HelloTimeout = TimeSpan.FromSeconds(15);
            ShutdownTimeout = TimeSpan.FromSeconds(120);
            KillTimeout = TimeSpan.FromSeconds(5);
        }
    }

    public sealed class VoiceScope {
        public string Owner { get; private set; }
        public long Generation { get; private set; }
        internal VoiceScope(string owner, long generation) { Owner = owner; Generation = generation; }
        internal Dictionary<string, object> Wire() { return VoiceJson.Object("owner", Owner, "generation", Generation); }
    }

    public sealed class VoiceResult {
        public string Event { get; private set; }
        public string Id { get; private set; }
        public long Sequence { get; private set; }
        public IDictionary<string, object> Data { get; private set; }
        public bool DeliveryRevoked { get; private set; }
        public string Error { get { return VoiceJson.Text(Data, "error"); } }
        public string StopError { get { return VoiceJson.Text(Data, "stop_error"); } }
        public string Transcript { get { return DeliveryRevoked ? null : VoiceJson.Text(Data, "text"); } }
        internal VoiceResult(string kind, string id, long sequence, IDictionary<string, object> data, bool revoked) {
            Event = kind; Id = id; Sequence = sequence; DeliveryRevoked = revoked;
            Data = new ReadOnlyDictionary<string, object>(new Dictionary<string, object>(data));
        }
    }

    public sealed class VoiceOperation {
        internal readonly TaskCompletionSource<VoiceResult> Source = new TaskCompletionSource<VoiceResult>();
        internal readonly VoiceBridgeClient Owner;
        int revoked;
        public string Id { get; private set; }
        public long Sequence { get; private set; }
        public string Kind { get; private set; }
        public VoiceScope Scope { get; private set; }
        public long HelperGeneration { get; private set; }
        public bool DeliveryRevoked { get { return Interlocked.CompareExchange(ref revoked, 0, 0) != 0; } }
        public Task<VoiceResult> Completion { get { return Source.Task; } }
        internal VoiceOperation(VoiceBridgeClient owner, string id, long sequence, string kind, VoiceScope scope, long generation) {
            Owner = owner; Id = id; Sequence = sequence; Kind = kind; Scope = scope; HelperGeneration = generation;
        }
        internal void Revoke() { Interlocked.Exchange(ref revoked, 1); }
    }

    public sealed class VoiceSpeechStarted {
        public VoiceOperation Operation { get; private set; }
        public IDictionary<string, object> Data { get; private set; }
        internal VoiceSpeechStarted(VoiceOperation operation, IDictionary<string, object> data) {
            Operation = operation;
            Data = new ReadOnlyDictionary<string, object>(new Dictionary<string, object>(data));
        }
    }

    public sealed class VoiceShutdownResult {
        public bool CleanupConfirmed { get; internal set; }
        public bool ExitObserved { get; internal set; }
        public int? ExitCode { get; internal set; }
        public bool ForcedKill { get; internal set; }
        public string Error { get; internal set; }
        public VoiceResult Receipt { get; internal set; }
    }

    // The fake seam owns no device or model. Closed is a content-free error code.
    public interface IVoiceBridgeTransport : IDisposable {
        event Action<string> LineReceived;
        event Action<string> Closed;
        Task<int> Exit { get; }
        long StderrCharacterCount { get; }
        void Start();
        Task SendAsync(string line);
        void CloseInput();
        void Kill();
    }

    internal static class VoiceJson {
        internal static Dictionary<string, object> Object(params object[] pairs) {
            var result = new Dictionary<string, object>();
            for (int i = 0; i < pairs.Length; i += 2) result.Add((string)pairs[i], pairs[i + 1]);
            return result;
        }
        internal static object Get(IDictionary<string, object> value, string key) {
            object item; return value != null && value.TryGetValue(key, out item) ? item : null;
        }
        internal static string Text(IDictionary<string, object> value, string key) { return Get(value, key) as string; }
        internal static long Integer(IDictionary<string, object> value, string key) {
            var item = Get(value, key);
            if (!(item is int) && !(item is long)) throw new InvalidOperationException("voice_protocol_invalid");
            return Convert.ToInt64(item);
        }
        internal static InvalidOperationException Error(string code) { return new InvalidOperationException(code); }
    }
}

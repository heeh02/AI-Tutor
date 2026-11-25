using System;
using System.Collections.Generic;
using System.Web.Script.Serialization;

// Test-owned process: no models, devices, network, or persistent files.
internal static class SyntheticHelper
{
    private static readonly JavaScriptSerializer Json = new JavaScriptSerializer();
    private static void Emit(object value) { Console.WriteLine(Json.Serialize(value)); Console.Out.Flush(); }
    public static int Main(string[] args)
    {
        bool eof = args.Length > 1 && args[1].EndsWith("synthetic-eof", StringComparison.Ordinal);
        Console.Error.Write("PRIVATE_STDERR_SENTINEL_" + new string('x', 131072));
        Console.Error.Flush();
        Emit(new { version = 1, @event = "hello", data = new { automatic_prepare = false, transport = "stdio_jsonl_v1" } });
        string line;
        while ((line = Console.ReadLine()) != null)
        {
            var request = Json.Deserialize<Dictionary<string, object>>(line);
            string op = Convert.ToString(request["op"]);
            if (op == "shutdown")
            {
                Emit(new { version = 1, @event = "shutdown", data = new { cleanup_confirmed = true } });
                return 0;
            }
            string name = op == "bind" ? "bound" : op == "status" ? "status" : "accepted";
            object data = op == "bind" ? (object)new { scope = request["scope"] }
                : op == "status" ? (object)new { backend = new { ready = false, phase = "idle", microphone_owner = "caller" } }
                : new { };
            Emit(new Dictionary<string, object> { { "version", 1 }, { "id", request["id"] },
                { "sequence", request["sequence"] }, { "event", name }, { "data", data } });
            if (eof && (op == "speak" || op == "transcribe")) return 0;
        }
        return 0;
    }
}

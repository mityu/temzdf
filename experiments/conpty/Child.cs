// The program inside the pseudoconsole: writes the probe sequences to its
// console, each after a marker `<<N>>`, then reads raw VT input for a few
// seconds and writes what arrived, in hex, to the file named by its first
// argument. Compiled by run.ps1 with the .NET Framework's csc (C# 5).
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

static class Child
{
    const int STD_INPUT_HANDLE = -10;
    const int STD_OUTPUT_HANDLE = -11;
    const uint ENABLE_PROCESSED_OUTPUT = 0x1;
    const uint ENABLE_VIRTUAL_TERMINAL_PROCESSING = 0x4;
    const uint DISABLE_NEWLINE_AUTO_RETURN = 0x8;
    const uint ENABLE_VIRTUAL_TERMINAL_INPUT = 0x200;

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr GetStdHandle(int n);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetConsoleMode(IntPtr h, uint mode);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetConsoleMode(IntPtr h, out uint mode);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetConsoleCP(uint cp);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetConsoleOutputCP(uint cp);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool WriteFile(IntPtr h, byte[] buf, int n, out int written, IntPtr ov);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool ReadFile(IntPtr h, byte[] buf, int n, out int read, IntPtr ov);

    // The probes: what a terminal-graphics program sends to its terminal.
    public static readonly string[] Probes = {
        "\x1b_Ga=T,f=24,s=1,v=1,i=7;AAAA\x1b\\",          // 1 kitty graphics: transmit and show 1x1 RGB
        "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\",      // 2 kitty graphics: query
        "\x1b[>1u",                                        // 3 kitty keyboard: push flags
        "\x1b[?u",                                         // 4 kitty keyboard: query flags
        "\x1b[?1000h\x1b[?1006h\x1b[?1016h",               // 5 mouse: SGR, SGR-pixels
        "\x1b[c",                                          // 6 DA1
        "\x1b[14t",                                        // 7 window size in pixels
        "\x1b[16t",                                        // 8 cell size in pixels
        "\x1b]52;c;aGVsbG8=\x07",                          // 9 OSC 52 clipboard
        "\x1b]7;file://host/C:/Users\x1b\\",               // 10 OSC 7 working directory
        "\x1b[?2026h\x1b[?2026l",                          // 11 synchronized output
        "\x1b[?2004h",                                     // 12 bracketed paste
        "\x1b[?2026$p",                                    // 13 DECRQM synchronized output
        "\x1bP+q544e\x1b\\",                               // 14 XTGETTCAP TN
        "\x1b[38;2;10;20;30mrgb\x1b[0m",                   // 15 truecolor SGR
        "\x1b]8;;https://example.com/\x1b\\link\x1b]8;;\x1b\\", // 16 OSC 8 hyperlink
    };

    static int Main(string[] args)
    {
        IntPtr hout = GetStdHandle(STD_OUTPUT_HANDLE);
        IntPtr hin = GetStdHandle(STD_INPUT_HANDLE);
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
        SetConsoleMode(hout, ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN);
        bool inOk = SetConsoleMode(hin, ENABLE_VIRTUAL_TERMINAL_INPUT);
        uint inMode;
        GetConsoleMode(hin, out inMode);

        var log = new StringBuilder();
        log.AppendLine("input mode set: " + inOk + " mode=0x" + inMode.ToString("x"));

        for (int i = 0; i < Probes.Length; i++)
        {
            Write(hout, "<<" + (i + 1) + ">>" + Probes[i] + "\r\n");
            Thread.Sleep(50);
        }
        Write(hout, "<<end>>\r\n");

        // Raw input until 5 s pass (the host writes at about 1.5 s).
        var start = DateTime.UtcNow;
        var reader = new Thread(() =>
        {
            var buf = new byte[4096];
            while (true)
            {
                int n;
                if (!ReadFile(hin, buf, buf.Length, out n, IntPtr.Zero) || n == 0) break;
                var hex = new StringBuilder();
                for (int k = 0; k < n; k++) hex.Append(buf[k].ToString("x2")).Append(' ');
                lock (log)
                {
                    log.AppendLine(((int)(DateTime.UtcNow - start).TotalMilliseconds).ToString().PadLeft(5) + "ms " + n + " bytes: " + hex
                        + " | " + Escape(buf, n));
                }
            }
        });
        reader.IsBackground = true;
        reader.Start();
        Thread.Sleep(5000);
        lock (log) File.WriteAllText(args[0], log.ToString());
        return 0;
    }

    static void Write(IntPtr h, string s)
    {
        var b = Encoding.UTF8.GetBytes(s);
        int w;
        WriteFile(h, b, b.Length, out w, IntPtr.Zero);
    }

    public static string Escape(byte[] b, int n)
    {
        var sb = new StringBuilder();
        for (int i = 0; i < n; i++)
        {
            byte c = b[i];
            if (c == 0x1b) sb.Append("\\e");
            else if (c == '\\') sb.Append("\\\\");
            else if (c >= 0x20 && c < 0x7f) sb.Append((char)c);
            else sb.Append("\\x").Append(c.ToString("x2"));
        }
        return sb.ToString();
    }
}

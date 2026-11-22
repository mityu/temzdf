// The terminal side: opens a pseudoconsole (CreatePseudoConsole with the
// flags given as the first argument, decimal; from the in-box kernel32, or
// from the conpty.dll named by the optional fifth argument, loaded with
// LoadLibrary, its OpenConsole.exe beside it), starts Child.exe in it,
// records every byte ConPTY writes to the output pipe and, at about 1.5 s,
// writes input a terminal would send. Usage:
//   Host.exe FLAGS CHILD_EXE CHILD_LOG HOST_LOG [CONPTY_DLL]
// Compiled by run.ps1 with the .NET Framework's csc (C# 5).
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

static class Host
{
    [StructLayout(LayoutKind.Sequential)]
    struct COORD { public short X; public short Y; }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct STARTUPINFO
    {
        public int cb;
        public string lpReserved, lpDesktop, lpTitle;
        public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2;
        public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct STARTUPINFOEX { public STARTUPINFO StartupInfo; public IntPtr lpAttributeList; }

    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }

    const uint EXTENDED_STARTUPINFO_PRESENT = 0x00080000;
    const int STARTF_USESTDHANDLES = 0x100;
    static readonly IntPtr PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = (IntPtr)0x20016;

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CreatePipe(out IntPtr read, out IntPtr write, IntPtr sa, int size);
    [DllImport("kernel32.dll", EntryPoint = "CreatePseudoConsole")]
    static extern int CreatePseudoConsoleInBox(COORD size, IntPtr input, IntPtr output, uint flags, out IntPtr hpc);
    [DllImport("kernel32.dll", EntryPoint = "ClosePseudoConsole")]
    static extern void ClosePseudoConsoleInBox(IntPtr hpc);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr LoadLibraryW(string path);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Ansi)]
    static extern IntPtr GetProcAddress(IntPtr module, string name);

    delegate int CreateFn(COORD size, IntPtr input, IntPtr output, uint flags, out IntPtr hpc);
    delegate void CloseFn(IntPtr hpc);
    static CreateFn createFn = CreatePseudoConsoleInBox;
    static CloseFn closeFn = ClosePseudoConsoleInBox;
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, int flags, ref IntPtr size);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, IntPtr attr, IntPtr value, IntPtr size, IntPtr prev, IntPtr retSize);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool CreateProcess(string app, string cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags,
        IntPtr env, string cwd, ref STARTUPINFOEX si, out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool ReadFile(IntPtr h, byte[] buf, int n, out int read, IntPtr ov);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool WriteFile(IntPtr h, byte[] buf, int n, out int written, IntPtr ov);

    // What a terminal sends: keys, mouse, and answers to the child's queries.
    static readonly string[][] Inputs = {
        new[] { "plain a", "a" },
        new[] { "kitty keyboard ctrl+a (CSI u)", "\x1b[97;5u" },
        new[] { "SGR mouse press+release", "\x1b[<0;100;200M\x1b[<0;100;200m" },
        new[] { "DA1 reply", "\x1b[?62;4;22c" },
        new[] { "kitty keyboard flags reply", "\x1b[?1u" },
        new[] { "kitty graphics reply", "\x1b_Gi=31;OK\x1b\\" },
        new[] { "window pixel size reply (14t)", "\x1b[4;800;1280t" },
        new[] { "bracketed paste", "\x1b[200~pasted\x1b[201~" },
        new[] { "focus in", "\x1b[I" },
        new[] { "DECRPM reply (2026)", "\x1b[?2026;2$y" },
        new[] { "XTGETTCAP reply", "\x1bP1+r544e=787465726d\x1b\\" },
        new[] { "plain z", "z" },
    };

    static int Main(string[] args)
    {
        uint flags = uint.Parse(args[0]);
        string childExe = args[1], childLog = args[2], hostLog = args[3];
        var log = new StringBuilder();
        var start = DateTime.UtcNow;
        if (args.Length > 4)
        {
            IntPtr mod = LoadLibraryW(args[4]);
            IntPtr c = mod == IntPtr.Zero ? IntPtr.Zero : GetProcAddress(mod, "CreatePseudoConsole");
            IntPtr d = mod == IntPtr.Zero ? IntPtr.Zero : GetProcAddress(mod, "ClosePseudoConsole");
            if (c == IntPtr.Zero || d == IntPtr.Zero)
            {
                log.AppendLine("cannot load " + args[4] + ": " + Marshal.GetLastWin32Error());
                File.WriteAllText(hostLog, log.ToString());
                return 1;
            }
            createFn = (CreateFn)Marshal.GetDelegateForFunctionPointer(c, typeof(CreateFn));
            closeFn = (CloseFn)Marshal.GetDelegateForFunctionPointer(d, typeof(CloseFn));
            log.AppendLine("using " + args[4]);
        }

        IntPtr inRead, inWrite, outRead, outWrite;
        CreatePipe(out inRead, out inWrite, IntPtr.Zero, 0);
        CreatePipe(out outRead, out outWrite, IntPtr.Zero, 0);
        IntPtr hpc;
        int hr = createFn(new COORD { X = 80, Y = 25 }, inRead, outWrite, flags, out hpc);
        log.AppendLine("CreatePseudoConsole(flags=0x" + flags.ToString("x") + ") = 0x" + hr.ToString("x8"));
        if (hr != 0) { File.WriteAllText(hostLog, log.ToString()); return 1; }
        CloseHandle(inRead);
        CloseHandle(outWrite);

        var size = IntPtr.Zero;
        InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref size);
        var si = new STARTUPINFOEX();
        si.StartupInfo.cb = Marshal.SizeOf(typeof(STARTUPINFOEX));
        // Without STARTF_USESTDHANDLES and NULL handles, a child of a
        // console host can inherit the host's own handles instead.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = Marshal.AllocHGlobal(size);
        InitializeProcThreadAttributeList(si.lpAttributeList, 1, 0, ref size);
        UpdateProcThreadAttribute(si.lpAttributeList, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, hpc,
            (IntPtr)IntPtr.Size, IntPtr.Zero, IntPtr.Zero);
        PROCESS_INFORMATION pi;
        string cmd = "\"" + childExe + "\" \"" + childLog + "\"";
        if (!CreateProcess(null, cmd, IntPtr.Zero, IntPtr.Zero, false, EXTENDED_STARTUPINFO_PRESENT,
                IntPtr.Zero, null, ref si, out pi))
        {
            log.AppendLine("CreateProcess failed: " + Marshal.GetLastWin32Error());
            File.WriteAllText(hostLog, log.ToString());
            return 1;
        }

        var output = new StringBuilder();
        var reader = new Thread(() =>
        {
            var buf = new byte[65536];
            while (true)
            {
                int n;
                if (!ReadFile(outRead, buf, buf.Length, out n, IntPtr.Zero) || n == 0) break;
                lock (output)
                {
                    output.AppendLine("--- " + Ms(start) + "ms, " + n + " bytes");
                    output.AppendLine(Escape(buf, n));
                }
            }
        });
        reader.IsBackground = true;
        reader.Start();

        Thread.Sleep(1500);
        foreach (var input in Inputs)
        {
            var b = Encoding.UTF8.GetBytes(input[1]);
            int w;
            WriteFile(inWrite, b, b.Length, out w, IntPtr.Zero);
            log.AppendLine(Ms(start).PadLeft(5) + "ms wrote " + input[0] + ": " + Escape(b, b.Length));
            Thread.Sleep(150);
        }

        WaitForSingleObject(pi.hProcess, 15000);
        Thread.Sleep(300);
        closeFn(hpc);
        reader.Join(3000);
        lock (output) log.AppendLine("=== output").Append(output.ToString());
        File.WriteAllText(hostLog, log.ToString());
        return 0;
    }

    static string Ms(DateTime start)
    {
        return ((int)(DateTime.UtcNow - start).TotalMilliseconds).ToString();
    }

    static string Escape(byte[] b, int n)
    {
        var sb = new StringBuilder();
        for (int i = 0; i < n; i++)
        {
            byte c = b[i];
            if (c == 0x1b) sb.Append("\\e");
            else if (c == '\\') sb.Append("\\\\");
            else if (c == '\n') sb.Append("\\n\n");
            else if (c >= 0x20 && c < 0x7f) sb.Append((char)c);
            else sb.Append("\\x").Append(c.ToString("x2"));
        }
        return sb.ToString();
    }
}

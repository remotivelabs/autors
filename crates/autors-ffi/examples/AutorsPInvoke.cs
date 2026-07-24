// .NET P/Invoke example for autors-ffi (covers every exported function in
// include/autors.h).
// Usage: place autors_ffi.dll next to the executable (or on PATH), then call
// AutorsExample.Demo().
using System;
using System.Runtime.InteropServices;

namespace Autors
{
    /// <summary>P/Invoke declarations for autors_ffi.dll. Strings are returned
    /// as IntPtr; use <see cref="AutorsNative.GetStringAndFree"/> to obtain a
    /// .NET string and free the native buffer.</summary>
    public static class AutorsNative
    {
        private const string Dll = "autors_ffi";

        public const int Ok = 0;
        public const int ErrInvalidArg = 1;
        public const int ErrNotFound = 2;
        public const int ErrParse = 3;
        public const int ErrInternal = 4;

        public const ulong AddressUnset = 0xFFFFFFFFUL;
        public const ulong AddressInvalid = 0xFFFFFFFFFFFFFFFFUL;

        // ---- Lifecycle ----
        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_project_new();

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern void autors_project_free(IntPtr handle);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern void autors_string_free(IntPtr s);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_last_error();

        // ---- Parse / write out ----
        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_project_parse_file(
            [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_project_parse_string(
            [MarshalAs(UnmanagedType.LPUTF8Str)] string text);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_project_write_string(IntPtr handle);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern int autors_project_save(
            IntPtr handle,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

        // ---- Queries ----
        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern int autors_project_module_count(IntPtr handle);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_project_module_name(IntPtr handle, int index);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern int autors_project_measurement_count(IntPtr handle);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_project_find_measurement(
            IntPtr handle,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string name);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_measurement_name(IntPtr meas);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_measurement_long_identifier(IntPtr meas);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_measurement_data_type(IntPtr meas);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_measurement_conversion_name(IntPtr meas);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern ulong autors_measurement_address(IntPtr meas);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_project_find_characteristic(
            IntPtr handle,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string name);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_characteristic_name(IntPtr ch);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern ulong autors_characteristic_address(IntPtr ch);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_characteristic_record_layout(IntPtr ch);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr autors_characteristic_conversion_name(IntPtr ch);

        // ---- Conversion ----
        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern int autors_measurement_to_physical(
            IntPtr handle,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string name,
            double raw,
            out double phys);

        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern int autors_measurement_to_raw(
            IntPtr handle,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string name,
            double physical,
            out double raw);

        // ---- Updates ----
        [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
        public static extern int autors_measurement_set_address(
            IntPtr handle,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string name,
            ulong addr);

        /// <summary>Take ownership of a string returned by the FFI and free it
        /// (returns null when the FFI returned NULL).</summary>
        public static string GetStringAndFree(IntPtr p)
        {
            if (p == IntPtr.Zero) return null;
            string s = Marshal.PtrToStringUTF8(p);
            autors_string_free(p);
            return s;
        }

        /// <summary>Details of the last FFI error on the current thread
        /// (empty string when there is no error).</summary>
        public static string LastError()
        {
            IntPtr p = autors_last_error();
            return p == IntPtr.Zero ? "" : Marshal.PtrToStringUTF8(p);
        }
    }

    /// <summary>Usage example: parse A2L → query → convert → update address →
    /// write out.</summary>
    public static class AutorsExample
    {
        private const string SampleA2l =
            "/begin PROJECT Demo \"demo project\"\n" +
            "/begin MODULE M1 \"module one\"\n" +
            "/begin COMPU_METHOD Conv_EngineSpeed \"rpm conv\" LINEAR \"%4.0\" \"rpm\"\n" +
            "COEFFS_LINEAR 2 3\n" +
            "/end COMPU_METHOD\n" +
            "/begin MEASUREMENT EngineSpeed \"engine speed\" UWORD Conv_EngineSpeed 1 0 0 10000\n" +
            "ECU_ADDRESS 0x1000\n" +
            "/end MEASUREMENT\n" +
            "/end MODULE\n" +
            "/end PROJECT\n";

        /// <summary>Full demo flow; returns the printed log (requires
        /// autors_ffi.dll to be loadable).</summary>
        public static string Demo()
        {
            var log = new System.Text.StringBuilder();
            IntPtr proj = AutorsNative.autors_project_parse_string(SampleA2l);
            if (proj == IntPtr.Zero)
                throw new InvalidOperationException("parse failed: " + AutorsNative.LastError());
            try
            {
                log.AppendLine($"modules={AutorsNative.autors_project_module_count(proj)} " +
                               $"measurements={AutorsNative.autors_project_measurement_count(proj)}");

                IntPtr meas = AutorsNative.autors_project_find_measurement(proj, "EngineSpeed");
                if (meas == IntPtr.Zero)
                    throw new InvalidOperationException("find failed: " + AutorsNative.LastError());
                log.AppendLine($"name={AutorsNative.GetStringAndFree(AutorsNative.autors_measurement_name(meas))} " +
                               $"address=0x{AutorsNative.autors_measurement_address(meas):X} " +
                               $"type={AutorsNative.GetStringAndFree(AutorsNative.autors_measurement_data_type(meas))}");

                double phys;
                int rc = AutorsNative.autors_measurement_to_physical(proj, "EngineSpeed", 10.0, out phys);
                if (rc != AutorsNative.Ok)
                    throw new InvalidOperationException($"to_physical failed ({rc}): " + AutorsNative.LastError());
                log.AppendLine($"to_physical(10)={phys}");

                rc = AutorsNative.autors_measurement_set_address(proj, "EngineSpeed", 0x2000);
                if (rc != AutorsNative.Ok)
                    throw new InvalidOperationException($"set_address failed ({rc}): " + AutorsNative.LastError());

                string text = AutorsNative.GetStringAndFree(AutorsNative.autors_project_write_string(proj));
                log.AppendLine($"write_string={text.Length} chars");
                return log.ToString();
            }
            finally
            {
                AutorsNative.autors_project_free(proj);
            }
        }
    }
}

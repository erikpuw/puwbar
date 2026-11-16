param(
    [ValidateSet('Preview','Normal','Crash')][string]$Mode = 'Normal',
    [string]$Exe = "$PSScriptRoot/../target/release/puwbar.exe",
    [switch]$Inspect
)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class PuwProbe {
    public delegate bool EnumProc(IntPtr h, IntPtr p);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left,Top,Right,Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Appbar { public uint Size; public IntPtr Hwnd; public uint Callback,Edge; public Rect Rect; public IntPtr Param; }
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr d);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr h, EnumProc p, IntPtr d);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h,StringBuilder b,int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h,StringBuilder b,int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out Rect r);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h,int n);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern bool SystemParametersInfo(uint a,uint p,ref Rect r,uint f);
    [DllImport("shell32.dll")] public static extern UIntPtr SHAppBarMessage(uint m,ref Appbar a);
    public static string Class(IntPtr h) { var b=new StringBuilder(256); GetClassName(h,b,b.Capacity); return b.ToString(); }
    public static string Title(IntPtr h) { var b=new StringBuilder(512); GetWindowText(h,b,b.Capacity); return b.ToString(); }
    public static IntPtr[] Find(string cls) { var r=new List<IntPtr>(); EnumWindows((h,p)=>{ if(Class(h)==cls)r.Add(h);return true;},IntPtr.Zero);return r.ToArray(); }
    public static string[] Buttons(IntPtr h) {var r=new List<string>();EnumChildWindows(h,(w,p)=>{if(Class(w)=="Button")r.Add(Title(w));return true;},IntPtr.Zero);return r.ToArray();}
    public static Rect Work() {var r=new Rect();SystemParametersInfo(48,0,ref r,0);return r;}
    public static ulong State() {var a=new Appbar();a.Size=(uint)Marshal.SizeOf(a);return SHAppBarMessage(4,ref a).ToUInt64();}
    public static void Restore(ulong state,Rect work) {var a=new Appbar();a.Size=(uint)Marshal.SizeOf(a);a.Param=(IntPtr)(long)state;SHAppBarMessage(10,ref a);SystemParametersInfo(47,0,ref work,2);}
}
'@

if (Get-Process puwbar -ErrorAction SilentlyContinue) { throw 'PuwBar đang chạy; không thử chồng lên phiên hiện tại.' }
$before = [PuwProbe]::State()
$work = [PuwProbe]::Work()
$bars = @([PuwProbe]::Find('Shell_TrayWnd')) + @([PuwProbe]::Find('Shell_SecondaryTrayWnd'))
$visibility = @{}
foreach ($bar in $bars) { $visibility[$bar.ToInt64()] = [PuwProbe]::IsWindowVisible($bar) }
$process = $null
try {
    $arguments = @('--test-seconds', $(if($Inspect){'120'}else{'8'}))
    if ($Mode -eq 'Preview') { $arguments += '--preview' }
    $process = Start-Process -FilePath (Resolve-Path $Exe) -ArgumentList $arguments -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 3
    $process.Refresh()
    if ($process.HasExited) { throw "PuwBar kết thúc sớm: $($process.ExitCode)" }
    $panels = @([PuwProbe]::Find('PuwBar.Window'))
    if ($panels.Count -lt 2) { throw 'Không có đủ dock và cụm trạng thái.' }
    $buttons = @($panels | ForEach-Object { [PuwProbe]::Buttons($_) })
    if ('Start' -notin $buttons) { throw 'Thiếu nút Start.' }
    if ($Inspect) {
        foreach ($panel in $panels) {
            $r = New-Object PuwProbe+Rect
            [PuwProbe]::GetWindowRect($panel,[ref]$r) | Out-Null
            [pscustomobject]@{Handle=$panel.ToInt64();Title=[PuwProbe]::Title($panel);Rect=$r;Buttons=@([PuwProbe]::Buttons($panel))} | ConvertTo-Json -Compress
        }
        $process.WaitForExit(125000) | Out-Null
        return
    }
    $visible = @($bars | Where-Object { [PuwProbe]::IsWindowVisible($_) }).Count
    if ($Mode -ne 'Preview' -and $visible -ne 0) { throw 'Taskbar Windows chưa ẩn hoàn toàn.' }
    if ($Mode -eq 'Preview' -and [PuwProbe]::State() -ne $before) { throw 'Preview đã thay đổi taskbar.' }
    $sample1 = Get-Process -Id $process.Id
    $cpu1 = $sample1.CPU
    Start-Sleep -Seconds 2
    $sample2 = Get-Process -Id $process.Id
    $cpuSeconds = [Math]::Round($sample2.CPU - $cpu1,4)
    $workingMb = [Math]::Round($sample2.WorkingSet64 / 1MB,2)
    if ($Mode -eq 'Crash') { Stop-Process -Id $process.Id -Force }
    else { $process.WaitForExit(10000) | Out-Null }
    Start-Sleep -Seconds 2
    if (Get-Process puwbar -ErrorAction SilentlyContinue) { throw 'Tiến trình chính hoặc guard chưa kết thúc.' }
    $after = [PuwProbe]::State()
    $afterWork = [PuwProbe]::Work()
    if ($after -ne $before) { throw "Trạng thái không được trả lại: $before -> $after" }
    foreach ($edge in 'Left','Top','Right','Bottom') {
        if ($afterWork.$edge -ne $work.$edge) { throw "Vùng làm việc lệch ở $edge" }
    }
    foreach ($bar in $bars) {
        if ([PuwProbe]::IsWindowVisible($bar) -ne $visibility[$bar.ToInt64()]) { throw 'Taskbar gốc chưa được trả lại đúng trạng thái.' }
    }
    [pscustomobject]@{Mode=$Mode;Result='PASS';Panels=$panels.Count;Buttons=$buttons.Count;VisibleNativeTaskbarsDuringRun=$visible;MainWorkingSetMB=$workingMb;CpuSecondsOver2Seconds=$cpuSeconds;Restored=$true} | ConvertTo-Json -Compress
} finally {
    if ($process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force; Start-Sleep -Seconds 1 }
    [PuwProbe]::Restore($before,$work)
    foreach ($bar in $bars) { [PuwProbe]::ShowWindow($bar, $(if($visibility[$bar.ToInt64()]){8}else{0})) | Out-Null }
}

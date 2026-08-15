$path = "C:\Users\Administrator\Desktop\temp\demo\SynaRoute\src-tauri\target\debug\build"
$files = Get-ChildItem $path -File | Select-Object -First 1
if ($files) {
    $file = $files[0]
    Write-Host "Testing file: $($file.FullName)"
    
    # Try fsutil
    Write-Host "`n=== fsutil hardlink list ==="
    fsutil hardlink list $file.FullName 2>&1
}

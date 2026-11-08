"""Build a test-only UIA reader using the runner's installed Windows SDK."""
import os
import subprocess
from pathlib import Path


def build(root):
    vswhere=Path(os.environ['ProgramFiles(x86)'])/'Microsoft Visual Studio/Installer/vswhere.exe'
    install=subprocess.check_output([str(vswhere),'-latest','-products','*','-property','installationPath'],text=True).strip()
    setup=Path(install)/'Common7/Tools/VsDevCmd.bat'
    if not install or not setup.is_file():raise RuntimeError('Windows SDK compiler is missing')
    source=Path(__file__).with_name('inspect_windows_selection.cpp').resolve()
    output=root/'inspect-windows-selection.exe'
    # All paths come from this checkout, vswhere or the owned temporary fixture.
    # A batch file preserves Windows command-line quoting through cmd /c.
    command=root/'build-selection.cmd'
    command.write_text('@echo off\ncall "'+str(setup)+'" -arch=x64\nif errorlevel 1 exit /b %errorlevel%\n'+
        subprocess.list2cmdline(['cl','/nologo','/EHsc','/MT',str(source),'/Fe:'+str(output),'/link','ole32.lib','oleaut32.lib','uiautomationcore.lib','uuid.lib'])+'\n',encoding='utf-8')
    result=subprocess.run(['cmd','/d','/c',str(command)],cwd=root,capture_output=True,text=True)
    if result.returncode:raise RuntimeError('Native UIA probe build failed: '+result.stdout+result.stderr)
    return output

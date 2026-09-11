#!/bin/bash
# 用构建好的 AetherOS ISO 创建测试虚拟机并启动
# 前提: ISO 已拷回宿主机项目根目录
VBOX="C:/Program Files/Oracle/VirtualBox/VBoxManage.exe"
ISO="D:/CBN-HT/Desktop/AI编程/除了dsh以外的项目/系统/电脑系统/Aether/aetheros-0.1-amd64.iso"

"$VBOX" createvm --name "AetherOS-Test" --ostype Linux_64 --register
"$VBOX" modifyvm "AetherOS-Test" --memory 2048 --cpus 2 --graphicscontroller vmsvga --audio-driver none
"$VBOX" storagectl "AetherOS-Test" --name "IDE" --add ide
"$VBOX" storageattach "AetherOS-Test" --storagectl "IDE" --port 1 --device 0 --type dvddrive --medium "$ISO"
"$VBOX" startvm "AetherOS-Test"
echo "=== AetherOS-Test 已启动（带窗口，可直接观看启动过程）==="

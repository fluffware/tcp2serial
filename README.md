# TCP to serial gateway

This is a program that allows access to serial ports by sending and
receiving data over TCP.

It can be run from the command line or started by systemd.
Select feature "systemd" to include support for systemd notification and logging. The option `--no-systemd` disables systemd functionality at runtime.

It has limited support for multiple TCP clients for serial protocols
that is request/response based. The most recent mesage sent from a TCP
client decides where the response is sent. If no data is transferred
within the switch delay time then another client is allowed to take
over the serial line.


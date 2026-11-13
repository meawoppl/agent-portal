`timescale 1ns/1ps
module counter_tb;
    reg clk = 0;
    reg reset = 1;
    wire [7:0] count;
    counter dut(clk, reset, count);
    always #5 clk = ~clk;
    initial begin
        $dumpfile("counter.vcd");
        $dumpvars(0, counter_tb);
        #12 reset = 0;
        #100;
        assert(count == 10) else $fatal(1, "Wrong count: %d", count);
        #388;
        $finish;
    end
    initial begin
        #1000;
        $fatal(1, "Watchdog timeout");
    end
endmodule

using System;
using System.Diagnostics;
using System.Threading;

class Hog
{
    static void Main(string[] args)
    {
        double burn1 = args.Length > 0 ? double.Parse(args[0]) : 12;
        double idle1 = args.Length > 1 ? double.Parse(args[1]) : 6;
        double burn2 = args.Length > 2 ? double.Parse(args[2]) : 12;
        double idle2 = args.Length > 3 ? double.Parse(args[3]) : 10;
        int threads = args.Length > 4 ? int.Parse(args[4]) : 2;

        Burn(burn1, threads);
        Thread.Sleep((int)(idle1 * 1000));
        Burn(burn2, threads);
        Thread.Sleep((int)(idle2 * 1000));
    }

    static void Burn(double seconds, int threads)
    {
        var deadline = DateTime.UtcNow.AddSeconds(seconds);
        var workers = new Thread[threads];
        for (int t = 0; t < threads; t++)
        {
            workers[t] = new Thread(() =>
            {
                long x = 0;
                while (DateTime.UtcNow < deadline)
                {
                    for (int i = 0; i < 1000000; i++) x += i;
                }
                if (x == -1) Console.WriteLine(x);
            });
            workers[t].IsBackground = false;
            workers[t].Start();
        }
        foreach (var w in workers) w.Join();
    }
}

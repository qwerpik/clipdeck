// DecompRefs: for each literal (args), find defined strings containing it, list xrefs,
// and decompile every function that references it. Output appended to $OUT file.
import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import ghidra.program.model.address.*;
import java.io.*;
import java.util.*;

public class DecompRefs extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        String out = args[0];
        PrintWriter pw = new PrintWriter(new FileWriter(out, true));
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        Set<Address> done = new HashSet<>();
        for (int a = 1; a < args.length; a++) {
            String arg = args[a];
            if (arg.startsWith("fn:")) {               // decompile function at address
                Address ad = toAddr(arg.substring(3));
                Function f = getFunctionContaining(ad);
                if (f != null && done.add(f.getEntryPoint())) dump(pw, di, f, "explicit " + arg);
                continue;
            }
            boolean exact = arg.startsWith("=");
            String needle = exact ? arg.substring(1) : arg;
            DataIterator it = currentProgram.getListing().getDefinedData(true);
            while (it.hasNext()) {
                Data d = it.next();
                if (!d.hasStringValue()) continue;
                Object v = d.getValue();
                if (v == null) continue;
                String s = v.toString();
                if (exact ? !s.equals(needle) : !s.contains(needle)) continue;
                pw.println("=== STRING " + d.getAddress() + " \"" + s.replace("\n","\\n") + "\"");
                for (Reference r : getReferencesTo(d.getAddress())) {
                    Function f = getFunctionContaining(r.getFromAddress());
                    pw.println("    ref from " + r.getFromAddress() + " in " + (f == null ? "?" : f.getName() + "@" + f.getEntryPoint()));
                    if (f != null && done.add(f.getEntryPoint())) dump(pw, di, f, "ref " + needle);
                }
            }
        }
        pw.close();
    }
    void dump(PrintWriter pw, DecompInterface di, Function f, String why) {
        DecompileResults res = di.decompileFunction(f, 120, monitor);
        pw.println("\n////////// FUNCTION " + f.getName() + " @ " + f.getEntryPoint() + "  (" + why + ")");
        if (res != null && res.getDecompiledFunction() != null) pw.println(res.getDecompiledFunction().getC());
        else pw.println("// decompile failed");
    }
}
